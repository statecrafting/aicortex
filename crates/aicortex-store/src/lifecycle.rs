//! The rest of a memory's life (spec 014 B-2 to B-6).
//!
//! A store that can only append becomes wrong over time and then confidently
//! recalls the wrong thing, which is the characteristic failure of memory
//! systems and the one users notice. The predecessor had exact dedup and no
//! other lifecycle at all (`openbrain://exact-dedup-only`). This module is
//! the rest: a repeated capture merges instead of duplicating (B-2), a
//! replaced claim is superseded rather than overwritten (B-3), a wrong claim
//! is corrected by a memory that says who corrected it (B-4), and a claim
//! that was true until a date expires (B-5).
//!
//! Four properties hold across all of it.
//!
//! **Nothing here deletes.** Every operation in this module is a status
//! change or an append; the only code that removes anything is
//! [`crate::erasure`], and it is a separate module because forgetting is a
//! different act with a different authority behind it (constitution XIV).
//!
//! **The record and its columns move together.** `status`, `superseded_by`
//! and `updated` are projections of the record (012 D-3), so every statement
//! that changes one rewrites the `record` in the same statement. There is no
//! path here that writes a column and leaves the document disagreeing with
//! it.
//!
//! **A batch is bounded and leased.** The expiry pass of B-5 takes a rahi
//! lease and writes through [`rahi_store::StoreHandle::fenced_txn`], so a
//! holder that has been superseded writes no row rather than racing the
//! holder that replaced it. Batches are bounded because a lease expires after
//! [`rahi_store::LEASE_TTL_SECONDS`] whether or not the work is finished.
//!
//! **A parameter is named, not numbered.** SQLite reads `$1` as a *named*
//! parameter and assigns its index by order of first appearance, not by the
//! digit in the name. A statement whose `SET` clause mentions `$3` before its
//! `WHERE` mentions `$1` therefore binds the caller's first value to `$3`,
//! silently, and matches no row. Every statement in this crate numbers its
//! parameters in ascending order of first appearance, which is what makes the
//! digits mean what they look like they mean. A later spec adding a statement
//! here inherits the rule and should assume nothing else about it.
//!
//! **Decay is not here.** B-6: importance decays by computation at read time
//! ([`aicortex_types::Importance::decayed_at`]), and no pass in this module
//! rewrites rows to decay them. A decay job would be a write amplification
//! proportional to the size of the store on every tick, which is the cost
//! that makes people turn decay off.

use aicortex_gate::Admitted;
use aicortex_types::{AicortexTime, Memory, MemoryId, MemoryKind, Provenance, Scope, Status};
use rahi_store::{Envelope, Outbox, Statement, StoreHandle, TxnBuilder, Value};
use rahi_types::Error;
use serde::Deserialize;

use crate::counters::Counters;
use crate::fingerprint;
use crate::memory_repo::MemoryRepo;
use crate::scope_repo::{ScopeId, seconds_to_sql};

/// The largest expiry batch a single leased pass will claim.
///
/// A lease lives [`rahi_store::LEASE_TTL_SECONDS`] and the pass must finish a
/// batch inside it or find its writes fenced out, so the ceiling is low
/// enough that a batch is a short transaction and the pass is simply run
/// again for the next one.
pub const MAX_EXPIRY_BATCH: u32 = 500;

/// What a capture did (B-2).
///
/// The caller is told which happened rather than having to infer it from a
/// row count, because an importer's "created 400, merged 3600" is the number
/// that tells a human whether a re-import did what they expected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Captured {
    /// A new row. The id is the one the candidate carried.
    Inserted(MemoryId),
    /// The same content was already here. The id is the existing row's, and
    /// the candidate's id was never used.
    Merged(MemoryId),
}

impl Captured {
    /// The id the store holds this content under, however it got there.
    #[must_use]
    pub const fn id(self) -> MemoryId {
        match self {
            Self::Inserted(id) | Self::Merged(id) => id,
        }
    }

    /// Whether this capture found the content already present.
    #[must_use]
    pub const fn is_merge(self) -> bool {
        matches!(self, Self::Merged(_))
    }
}

/// What one expiry pass did (B-5).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Expired {
    /// How many memories moved to [`Status::Expired`].
    pub memories: u64,
    /// Whether more were due than this batch claimed, so the caller knows to
    /// run again rather than having to ask.
    pub more: bool,
}

/// Supersession, correction, expiry, and the merge half of capture.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Lifecycle {
    repo: MemoryRepo,
}

// A failed comparison writes NULL into a NOT NULL column, aborting the
// entire transaction including source and outbox rows. A zero-row UPDATE
// alone would silently commit those side effects after an erasure.
// ?N is a numbered SQLite binding even when first mentioned out of order;
// $N is a named binding whose position is its first occurrence.
const MERGE_SQL: &str = "UPDATE memory
    SET record = CASE WHEN record = ?5 AND status <> 'erased' THEN ?1 ELSE NULL END,
        updated = ?2
    WHERE scope_id = ?3 AND id = ?4";

/// One source of a memory, appended at the next free ordinal (B-2).
///
/// The ordinal is computed by the statement rather than by the caller,
/// because the caller is staging into a transaction it has not run: a value
/// read before the batch could be stale by the time the batch commits, and
/// two concurrent merges would then write the same ordinal and collide on the
/// primary key. `MAX(ordinal) + 1` is evaluated inside the transaction, where
/// it cannot be.
const SOURCE_SQL: &str = "INSERT INTO memory_source (
        memory_id, ordinal, scope_id, source_system, source_external_id,
        source_locator, captured_at, ingested_at, extractor_name, extractor_version
    ) VALUES (
        ?1,
        (SELECT COALESCE(MAX(ordinal), -1) + 1 FROM memory_source WHERE memory_id = ?1),
        ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9
    )";

const SUPERSEDE_SQL: &str = "UPDATE memory
    SET status = 'superseded', superseded_by = ?1,
        record = CASE WHEN record = ?6 AND status <> 'erased' AND NOT EXISTS (
            WITH RECURSIVE successors(id) AS (
                VALUES (?1) UNION
                SELECT m.superseded_by FROM memory m JOIN successors s ON m.id = s.id
                WHERE m.scope_id = ?4 AND m.superseded_by IS NOT NULL
            ) SELECT 1 FROM successors WHERE id = ?5
        ) THEN ?2 ELSE NULL END, updated = ?3
    WHERE scope_id = ?4 AND id = ?5";

const SUCCESSOR_SQL: &str = "SELECT superseded_by AS id FROM memory
    WHERE scope_id = $1 AND id = $2 AND superseded_by IS NOT NULL";

const PREDECESSOR_SQL: &str = "SELECT id FROM memory
    WHERE scope_id = $1 AND superseded_by = $2";

const VALID_UNTIL_SQL: &str = "UPDATE memory SET valid_until = $1
    WHERE scope_id = $2 AND id = $3 AND status <> 'erased'";

/// The memories of one scope that are due to expire, oldest deadline first.
///
/// One more than the batch is read, which is how [`Expired::more`] is
/// answered without a second count.
const DUE_SQL: &str = "SELECT id, kind FROM memory
    WHERE scope_id = $1 AND status = 'active'
      AND valid_until IS NOT NULL AND valid_until <= $2
    ORDER BY valid_until, id LIMIT $3";

/// The rows a scope holds that are not erased, for the re-digest of 012 D-4.
const REDIGEST_READ_SQL: &str = "SELECT id, record FROM memory
    WHERE scope_id = $1 AND status <> 'erased' AND fingerprint_version < 1 ORDER BY id LIMIT $2";

const REDIGEST_WRITE_SQL: &str = "UPDATE memory SET fingerprint = $1,
        fingerprint_version = CASE WHEN record = $2 THEN 1 ELSE NULL END
    WHERE scope_id = $3 AND id = $4 AND status <> 'erased'
      AND fingerprint_version < 1";

#[derive(Debug, Deserialize)]
struct IdRow {
    id: String,
}

#[derive(Debug, Deserialize)]
struct DueRow {
    id: String,
    kind: String,
}

#[derive(Debug, Deserialize)]
struct RecordRow {
    id: String,
    record: String,
}

impl Lifecycle {
    /// A lifecycle over the default memory repository.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The same, over a repository with a different body ceiling (012 B-8).
    #[must_use]
    pub const fn with_repo(repo: MemoryRepo) -> Self {
        Self { repo }
    }

    /// Stage a capture, merging it if this scope already holds the content
    /// (B-2).
    ///
    /// The uniqueness read goes through the leader
    /// ([`MemoryRepo::fingerprint_holder`], 012 B-5): the answer decides
    /// whether a row is inserted or merged, and a stale replica would admit a
    /// duplicate that the unique index then refuses at commit, turning a
    /// clean decision into a constraint violation.
    ///
    /// A merge moves `updated` forward, increments `importance.uses`, appends
    /// the new provenance as a further source row, and unions the title and
    /// the media the new capture brought. It does not touch the body: the
    /// bodies are identical by construction, because the fingerprint is over
    /// the normalized body and the fingerprints matched.
    ///
    /// Nothing is executed. Everything lands in `txn`, so a capture is one
    /// transaction whichever way it went (constitution XI).
    /// A concurrent record change aborts that transaction at commit; the
    /// caller must re-read and stage a fresh capture before retrying.
    ///
    /// # Errors
    ///
    /// The store's error from the uniqueness read; [`Error::Integrity`] when
    /// the existing row does not read back as a memory or has gone between
    /// the read and the merge; whatever [`MemoryRepo::insert`] refuses.
    pub async fn capture(
        &self,
        store: &StoreHandle,
        txn: &mut TxnBuilder,
        admitted: &Admitted,
        work: &Envelope,
    ) -> Result<Captured, Error> {
        let memory = admitted.memory();
        let scope_id = ScopeId::of(&memory.scope);
        let digest = fingerprint::of_memory(memory);
        let holder = self
            .repo
            .fingerprint_holder(store, &memory.scope, &digest)
            .await?;

        let Some(existing_id) = holder else {
            let provenance = memory.provenance.clone();
            self.repo.insert(txn, admitted, &provenance, work)?;
            stage_source(txn, &scope_id, memory.id, &memory.provenance)?;
            return Ok(Captured::Inserted(memory.id));
        };

        let Some(mut existing) = self.repo.get(store, &memory.scope, existing_id).await? else {
            return Err(Error::Integrity(format!(
                "memory {existing_id} holds a fingerprint but not a row"
            )));
        };
        if existing.status == Status::Erased {
            return Err(Error::Conflict(format!(
                "memory {existing_id} was erased before merge"
            )));
        }
        let expected = serde_json::to_string(&existing).map_err(|error| {
            Error::Validation(format!("memory {existing_id} does not serialize: {error}"))
        })?;
        merge_into(&mut existing, memory);
        let record = serde_json::to_string(&existing).map_err(|error| {
            Error::Validation(format!("memory {existing_id} does not serialize: {error}"))
        })?;
        txn.push(Statement::with_params(
            MERGE_SQL,
            vec![
                Value::from(record),
                Value::Integer(seconds_to_sql(existing.updated)),
                Value::from(&scope_id),
                Value::from(existing_id.to_string()),
                Value::from(expected),
            ],
        ));
        stage_source(txn, &scope_id, existing_id, &memory.provenance)?;
        // The work is staged either way: a merge changed the row, so whatever
        // the capture implies downstream (re-embedding, re-indexing) is owed
        // for the merged row as much as for a new one. Losing it here would
        // be the non-atomic second half constitution XI exists to prevent.
        Outbox::stage(txn, work);
        Ok(Captured::Merged(existing_id))
    }

    /// Stage the supersession of `old` by `new` (B-3).
    ///
    /// Staged into the caller's transaction, which is what lets the insert of
    /// the new memory and the demotion of the old one be one commit: a
    /// supersession that landed without its successor would leave a scope
    /// with a claim pointing at a row that does not exist.
    ///
    /// Acyclicity is asserted on write rather than assumed. The chain from
    /// `new` is walked to its end before anything is staged, and a walk that
    /// reaches `old` is refused. `new` is usually a row this transaction is
    /// inserting and so has no successor at all, in which case the walk is one
    /// read that finds nothing; the check costs what it costs on the path
    /// where a cycle is actually possible.
    /// The committing statement repeats the reachability check and compares
    /// the old record, so concurrent erasure or supersession cannot make the
    /// staged update silently corrupt the row or its counters.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when `old` and `new` are the same memory, or
    /// when the supersession would close a cycle; [`Error::Integrity`] when
    /// `old` is not a row of this scope or does not read back;
    /// [`Error::Conflict`] when `old` has already been erased.
    pub async fn supersede(
        &self,
        store: &StoreHandle,
        txn: &mut TxnBuilder,
        scope: &Scope,
        old: MemoryId,
        new: MemoryId,
        at: AicortexTime,
    ) -> Result<(), Error> {
        if old == new {
            return Err(Error::Validation(format!(
                "memory {old} cannot supersede itself"
            )));
        }
        let mut walker = new;
        let mut steps = 0_u32;
        while let Some(next) = self.successor(store, scope, walker).await? {
            if next == old {
                return Err(Error::Validation(format!(
                    "superseding {old} by {new} would close a supersession cycle"
                )));
            }
            walker = next;
            steps = steps.saturating_add(1);
            if steps > MAX_CHAIN {
                return Err(Error::Integrity(format!(
                    "the supersession chain from {new} is longer than {MAX_CHAIN} and is \
                     presumed cyclic"
                )));
            }
        }

        let Some(mut memory) = self.repo.get(store, scope, old).await? else {
            return Err(Error::Integrity(format!(
                "memory {old} is not a row of this scope"
            )));
        };
        if memory.status == Status::Erased {
            return Err(Error::Conflict(format!(
                "memory {old} has been erased and cannot be superseded"
            )));
        }
        let expected = serde_json::to_string(&memory).map_err(|error| {
            Error::Validation(format!("memory {old} does not serialize: {error}"))
        })?;
        let was = memory.status;
        memory.status = Status::Superseded(new);
        memory.updated = at;
        let record = serde_json::to_string(&memory).map_err(|error| {
            Error::Validation(format!("memory {old} does not serialize: {error}"))
        })?;
        let scope_id = ScopeId::of(scope);
        txn.push(Statement::with_params(
            SUPERSEDE_SQL,
            vec![
                Value::from(new.to_string()),
                Value::from(record),
                Value::Integer(seconds_to_sql(at)),
                Value::from(&scope_id),
                Value::from(old.to_string()),
                Value::from(expected),
            ],
        ));
        Counters::adjust(txn, &scope_id, memory.kind, &was, -1);
        Counters::adjust(txn, &scope_id, memory.kind, &memory.status, 1);
        Ok(())
    }

    /// Stage a correction: insert the correcting memory and supersede the
    /// memory it corrects (B-4).
    ///
    /// The correction is an ordinary capture, so it merges if the same
    /// correction has been made before; the supersession is staged against
    /// whichever row that capture settled on.
    ///
    /// Three things are checked rather than trusted, because a correction
    /// that does not say what it corrects is a second claim rather than a
    /// correction:
    ///
    /// - it is a [`MemoryKind::Correction`];
    /// - its provenance names `old` in `derived_from`, which is what makes
    ///   the correction's own origin auditable;
    /// - a correction written by an agent is an
    ///   [`aicortex_types::TrustClass::Assertion`]. A non-human actor cannot
    ///   produce an instruction-grade correction here. The record of 011
    ///   already makes a promotion impossible without a human decision, so
    ///   this is the same rule stated where a correction is made rather than
    ///   a second, weaker copy of it.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] for any of the three, and whatever
    /// [`Self::capture`] and [`Self::supersede`] refuse.
    pub async fn correct(
        &self,
        store: &StoreHandle,
        txn: &mut TxnBuilder,
        correction: &Admitted,
        old: MemoryId,
        work: &Envelope,
    ) -> Result<Captured, Error> {
        let memory = correction.memory();
        if memory.kind != MemoryKind::Correction {
            return Err(Error::Validation(format!(
                "memory {} corrects {old} but is a {} rather than a correction",
                memory.id,
                memory.kind.label()
            )));
        }
        if !memory.provenance.derived_from.contains(&old) {
            return Err(Error::Validation(format!(
                "the correction {} does not name {old} in derived_from",
                memory.id
            )));
        }
        if !memory.actor.is_human() && memory.trust.is_actionable() {
            return Err(Error::Validation(format!(
                "the correction {} was written by {} and cannot be instruction grade",
                memory.id,
                memory.actor.kind.label()
            )));
        }
        let scope = memory.scope.clone();
        let at = memory.created;
        let captured = self.capture(store, txn, correction, work).await?;
        self.supersede(store, txn, &scope, old, captured.id(), at)
            .await?;
        Ok(captured)
    }

    /// The memory that superseded `id`, when one did (B-3).
    ///
    /// The forward direction of the chain. `query_consistent`, the leader:
    /// the answer guards the acyclicity assertion in [`Self::supersede`], and
    /// a stale replica that had not yet seen the last supersession would let
    /// a cycle through.
    ///
    /// # Errors
    ///
    /// The store's error, or [`Error::Integrity`] when the column holds
    /// something that is not a memory id.
    pub async fn successor(
        &self,
        store: &StoreHandle,
        scope: &Scope,
        id: MemoryId,
    ) -> Result<Option<MemoryId>, Error> {
        let rows: Vec<IdRow> = store
            .query_consistent(
                SUCCESSOR_SQL,
                vec![
                    Value::from(&ScopeId::of(scope)),
                    Value::from(id.to_string()),
                ],
            )
            .await?;
        rows.into_iter()
            .next()
            .map(|row| parse_id(&row.id))
            .transpose()
    }

    /// The memory `id` superseded, when it superseded one (B-3).
    ///
    /// The backward direction, which is what makes the chain walkable in both
    /// directions: from a memory a client holds, "what did this replace?" is
    /// as answerable as "what replaced this?".
    ///
    /// # Errors
    ///
    /// The store's error, or [`Error::Integrity`] when a row holds an id that
    /// is not a memory id.
    pub async fn predecessor(
        &self,
        store: &StoreHandle,
        scope: &Scope,
        id: MemoryId,
    ) -> Result<Option<MemoryId>, Error> {
        let rows: Vec<IdRow> = store
            .query_consistent(
                PREDECESSOR_SQL,
                vec![
                    Value::from(&ScopeId::of(scope)),
                    Value::from(id.to_string()),
                ],
            )
            .await?;
        rows.into_iter()
            .next()
            .map(|row| parse_id(&row.id))
            .transpose()
    }

    /// Stage the deadline a memory is true until (B-5).
    ///
    /// `None` clears it, which is how a claim that turned out to be permanent
    /// stops being swept. Staged, like everything else here, so a deadline
    /// set as part of a capture commits with it.
    pub fn set_valid_until(
        txn: &mut TxnBuilder,
        scope: &Scope,
        id: MemoryId,
        until: Option<AicortexTime>,
    ) {
        txn.push(Statement::with_params(
            VALID_UNTIL_SQL,
            vec![
                Value::from(until.map(seconds_to_sql)),
                Value::from(&ScopeId::of(scope)),
                Value::from(id.to_string()),
            ],
        ));
    }

    /// Move one bounded batch of due memories to [`Status::Expired`] (B-5).
    ///
    /// Takes the scope's lifecycle lease and writes through
    /// [`rahi_store::StoreHandle::fenced_txn`], so a pass that has been
    /// superseded by another holder affects no row rather than racing it. The
    /// batch is bounded because the lease expires after
    /// [`rahi_store::LEASE_TTL_SECONDS`] whether the work is finished or not;
    /// [`Expired::more`] tells the caller to come back rather than tempting it
    /// to raise the bound.
    ///
    /// Expiry is a status change and never a delete: a memory that stopped
    /// being true is still a true record of what was believed, and erasing it
    /// is a different act with a different authority ([`crate::erasure`]).
    ///
    /// The counter moves are in the same fenced batch as the status change,
    /// derived from the same predicate rather than from the row list this
    /// pass read a moment earlier. That is what makes them right under a
    /// concurrent write: all three statements are evaluated in one
    /// transaction, and the two counter statements run before the update that
    /// changes what they count.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when `batch` is zero or above
    /// [`MAX_EXPIRY_BATCH`]; [`Error::Conflict`] when the lease was
    /// superseded; the store's error otherwise.
    pub async fn expire_due(
        &self,
        store: &StoreHandle,
        scope: &Scope,
        now: AicortexTime,
        batch: u32,
    ) -> Result<Expired, Error> {
        if batch == 0 || batch > MAX_EXPIRY_BATCH {
            return Err(Error::Validation(format!(
                "an expiry batch of {batch} is not between 1 and {MAX_EXPIRY_BATCH}"
            )));
        }
        let scope_id = ScopeId::of(scope);
        let probe = i64::from(batch).saturating_add(1);
        let due: Vec<DueRow> = store
            .query_consistent(
                DUE_SQL,
                vec![
                    Value::from(&scope_id),
                    Value::Integer(seconds_to_sql(now)),
                    Value::Integer(probe),
                ],
            )
            .await?;
        if due.is_empty() {
            return Ok(Expired::default());
        }
        let more = u32::try_from(due.len()).is_ok_and(|read| read > batch);
        let ids: Vec<String> = due
            .into_iter()
            .take(batch as usize)
            .map(|row| row.id)
            .collect();
        let kinds = kinds_of(&ids, store, &scope_id).await?;

        // The 'expired' counter row may not exist yet, and a counter upsert is
        // an INSERT, which cannot be fenced. Materializing it at zero first is
        // idempotent and harmless on its own: a crash between the two leaves a
        // counter row reading zero, which is what it would read anyway.
        let mut prepare = TxnBuilder::new();
        for kind in &kinds {
            Counters::adjust(&mut prepare, &scope_id, *kind, &Status::Expired, 0);
            Counters::adjust(&mut prepare, &scope_id, *kind, &Status::Active, 0);
        }
        if !prepare.is_empty() {
            store.txn(prepare.into_statements()).await?;
        }

        let lease = store.lease(&lifecycle_lease_key(&scope_id)).await?;
        let statements = expiry_statements(&scope_id, &ids, &kinds, now);
        let result = store.fenced_txn(&lease, statements).await;
        lease.release().await;
        let results = result?;
        // The status change is the last statement of the batch, and its row
        // count is what actually moved rather than what was read as due.
        let memories = results.last().map_or(0, |row| row.rows_affected);
        Ok(Expired { memories, more })
    }

    /// Recompute the `fingerprint` column of a bounded batch of rows under
    /// the digest of B-1 (012 D-4).
    ///
    /// 012 computed the column over the memory's kind and body text as
    /// written and recorded that 014 would carry the re-digest. It is a
    /// backfill rather than a SQL migration because BLAKE3 has no spelling in
    /// SQLite: the digest is computed in this process from each row's own
    /// record, which is the source of truth for what the memory says (012
    /// D-3), and written back in one transaction.
    ///
    /// Bounded and resumable: a row at the current fingerprint version is
    /// not selected. The digest and version commit together, and a changed
    /// record aborts the batch instead of reporting false convergence.
    ///
    /// # Errors
    ///
    /// The store's error, or [`Error::Integrity`] when a stored record does
    /// not read back as a memory.
    pub async fn redigest(
        &self,
        store: &StoreHandle,
        scope: &Scope,
        batch: u32,
    ) -> Result<u64, Error> {
        let scope_id = ScopeId::of(scope);
        if batch == 0 || batch > crate::memory_repo::MAX_PAGE_ROWS {
            return Err(Error::Validation(
                "redigest requires a batch between 1 and 500".into(),
            ));
        }
        let rows: Vec<RecordRow> = store
            .query_consistent(
                REDIGEST_READ_SQL,
                vec![Value::from(&scope_id), Value::Integer(i64::from(batch))],
            )
            .await?;
        let mut txn = TxnBuilder::new();
        for row in rows {
            let memory: Memory = serde_json::from_str(&row.record).map_err(|error| {
                Error::Integrity(format!("a stored memory does not read back: {error}"))
            })?;
            let digest = fingerprint::of_memory(&memory);
            txn.push(Statement::with_params(
                REDIGEST_WRITE_SQL,
                vec![
                    Value::from(digest),
                    Value::from(row.record),
                    Value::from(&scope_id),
                    Value::from(row.id),
                ],
            ));
        }
        if !txn.is_empty() {
            let results = store.txn(txn.into_statements()).await?;
            let changed = results.iter().map(|result| result.rows_affected).sum();
            if changed == 0 {
                let pending: Vec<RecordRow> = store
                    .query_consistent(
                        REDIGEST_READ_SQL,
                        vec![Value::from(&scope_id), Value::Integer(1)],
                    )
                    .await?;
                if !pending.is_empty() {
                    return Err(Error::Conflict(
                        "redigest batch moved concurrently; retry remaining work".into(),
                    ));
                }
            }
            return Ok(changed);
        }
        Ok(0)
    }
}

/// The longest supersession chain the acyclicity walk will follow.
///
/// A chain longer than this is not a legitimate history, it is a cycle the
/// walk has not closed yet, and reporting it is better than looping. The
/// bound is deliberately far above anything a real correction history reaches.
const MAX_CHAIN: u32 = 10_000;

/// The lease key the lifecycle passes of one scope serialize on.
///
/// Per scope rather than global: two subjects' expiry passes have nothing to
/// say to each other, and one global lease would make the pass a queue.
#[must_use]
pub fn lifecycle_lease_key(scope: &ScopeId) -> String {
    format!("aicortex.lifecycle.{scope}")
}

/// Fold a repeated capture into the row that already holds the content (B-2).
///
/// The body is not touched: the fingerprints matched, so the normalized
/// bodies are equal by construction. What a repeat can legitimately bring is
/// a title the first capture did not have, media the first capture did not
/// reference, and the fact of having been seen again.
fn merge_into(existing: &mut Memory, repeat: &Memory) {
    existing.importance = existing.importance.used_at(repeat.created);
    if existing.updated < repeat.created {
        existing.updated = repeat.created;
    }
    if existing.body.title.is_none() {
        existing.body.title.clone_from(&repeat.body.title);
    }
    for media in &repeat.body.media {
        if !existing.body.media.contains(media) {
            existing.body.media.push(media.clone());
        }
    }
}

/// Stage one row in the per-capture source log (B-2).
fn stage_source(
    txn: &mut TxnBuilder,
    scope: &ScopeId,
    memory: MemoryId,
    provenance: &Provenance,
) -> Result<(), Error> {
    let (extractor_name, extractor_version) =
        provenance
            .extractor
            .as_ref()
            .map_or((None, None), |extractor| {
                (
                    Some(extractor.name.clone()),
                    Some(extractor.version.clone()),
                )
            });
    txn.push(Statement::with_params(
        SOURCE_SQL,
        vec![
            Value::from(memory.to_string()),
            Value::from(scope),
            Value::from(provenance.source.system.as_str()),
            Value::from(provenance.source.external_id.clone()),
            Value::from(provenance.source.locator.clone()),
            Value::Integer(seconds_to_sql(provenance.captured_at)),
            Value::Integer(seconds_to_sql(provenance.ingested_at)),
            Value::from(extractor_name),
            Value::from(extractor_version),
        ],
    ));
    Ok(())
}

/// The distinct kinds among a batch of ids, so the counter moves name them.
async fn kinds_of(
    ids: &[String],
    store: &StoreHandle,
    scope: &ScopeId,
) -> Result<Vec<MemoryKind>, Error> {
    let rows: Vec<DueRow> = store
        .query_consistent(
            format!(
                "SELECT DISTINCT id, kind FROM memory WHERE scope_id = $1 AND id IN ({})",
                placeholders(ids.len(), 2)
            ),
            std::iter::once(Value::from(scope))
                .chain(ids.iter().map(|id| Value::from(id.as_str())))
                .collect(),
        )
        .await?;
    let mut kinds: Vec<MemoryKind> = Vec::new();
    for row in rows {
        let kind = MemoryKind::all()
            .into_iter()
            .find(|kind| kind.label() == row.kind)
            .ok_or_else(|| {
                Error::Integrity(format!("memory holds an unknown kind {}", row.kind))
            })?;
        if !kinds.contains(&kind) {
            kinds.push(kind);
        }
    }
    Ok(kinds)
}

/// The three-part fenced batch of one expiry pass (B-5).
///
/// Order matters and is the whole of the correctness argument: the two
/// counter statements count the rows the update is about to change, and they
/// run before it changes them. Every statement is an `UPDATE`, which is what
/// [`rahi_store::Statement::fenced`] accepts, and each carries the scope in
/// its own predicate (012 B-3).
fn expiry_statements(
    scope: &ScopeId,
    ids: &[String],
    kinds: &[MemoryKind],
    now: AicortexTime,
) -> Vec<Statement> {
    let list = placeholders(ids.len(), 2);
    let id_values = || ids.iter().map(|id| Value::from(id.as_str()));
    let mut statements = Vec::with_capacity(kinds.len().saturating_mul(2).saturating_add(1));

    for kind in kinds {
        let claimed = format!(
            "(SELECT COUNT(*) FROM memory WHERE scope_id = $1 AND kind = $2 \
             AND status = 'active' AND id IN ({list}))"
        );
        let params = || {
            [Value::from(scope), Value::from(kind.label())]
                .into_iter()
                .chain(id_values())
                .collect::<Vec<Value>>()
        };
        statements.push(Statement::with_params(
            format!(
                "UPDATE scope_counter SET count = max(0, count - {claimed}) \
                 WHERE scope_id = $1 AND kind = $2 AND status = 'active'"
            ),
            params(),
        ));
        statements.push(Statement::with_params(
            format!(
                "UPDATE scope_counter SET count = count + {claimed} \
                 WHERE scope_id = $1 AND kind = $2 AND status = 'expired'"
            ),
            params(),
        ));
    }

    // `record` is rewritten by the same statement that moves the column, so
    // the document and its projection cannot disagree (012 D-3). `json_set`
    // is SQLite's own, which keeps the rewrite inside the transaction rather
    // than requiring a read, a decode and a second round-trip per row.
    statements.push(Statement::with_params(
        format!(
            "UPDATE memory SET status = 'expired', updated = ?1, \
             record = json_set(record, '$.status.state', 'expired', '$.updated', ?1) \
             WHERE scope_id = ?2 AND status = 'active' AND id IN ({list})"
        ),
        std::iter::once(Value::Integer(seconds_to_sql(now)))
            .chain(std::iter::once(Value::from(scope)))
            .chain(ids.iter().map(|id| Value::from(id.as_str())))
            .collect(),
    ));
    statements
}

/// `$n, $n+1, ...` for a list bound after `offset` earlier parameters.
fn placeholders(count: usize, offset: usize) -> String {
    (0..count)
        .map(|index| format!("${}", index.saturating_add(offset).saturating_add(1)))
        .collect::<Vec<String>>()
        .join(", ")
}

/// Read a stored id back as the memory id it is.
fn parse_id(id: &str) -> Result<MemoryId, Error> {
    MemoryId::parse(id).map_err(|error| {
        Error::Integrity(format!(
            "memory holds an id that is not a memory id: {error}"
        ))
    })
}
