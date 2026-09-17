//! Forgetting, and proving it reached everything (spec 014 B-7 to B-9, B-11).
//!
//! Constitution XIII names a tension and this module is where it is resolved:
//! the decision chain is append-only and hash-linked, so anything written to
//! it is there forever, and a subject has a right to erasure. Both hold at
//! once provided content never enters the chain. What the chain records about
//! an erasure is that it happened, to which scope, on whose authority, and
//! how many derivative rows went; what it never records is the body, a hash
//! of the body, or anything a body could be guessed against.
//!
//! The last of those is the reason [`crate::DecisionKey`] exists. Spec 013
//! B-9 writes the digest of a refused or quarantined body into the chain
//! *keyed*, with the key in an application-store row, so destroying the row
//! leaves the chained digest an opaque value that cannot be confirmed against
//! a guess. B-11 is the other half of that bargain and is frozen on this
//! file: an erasure destroys the keys of every Decision about the erased
//! memory or scope, in the same transaction as the rest of the erasure.
//!
//! # What erasure reaches
//!
//! [`Eraser::erase`] removes, in one transaction: the body, every derivative
//! row this schema version carries, every derivation row naming the memory as
//! a child, its provenance detail, its per-capture source log, and its
//! Decision digest keys. What it leaves is a tombstone: the id, the scope,
//! the kind, the timestamps, and [`Status::Erased`]. A hard delete would
//! break referential integrity from derived memories and from recall traces
//! and produce a system that cannot explain why a citation is missing (D-1);
//! the tombstone carries no content, which is what the right actually
//! requires.
//!
//! # What erasure does not reach, and says so
//!
//! Erasing a memory does not erase what was derived from it (B-8): a derived
//! summary may be independently valuable, and cascading by default would make
//! one erasure destroy work nobody asked about. Derivatives are *marked*
//! `origin_erased` instead, so a recall trace can say the citation's origin
//! is gone rather than present the derived claim as if its source still
//! stood. Cascading is an explicit flag on the call and is reported in the
//! Decision.
//!
//! A backup taken *before* an erasure still holds everything the erasure
//! removed, including the digest keys, until that backup is discarded. That
//! is a property of backups rather than a gap in this module, and it is
//! stated in [`Erased::caveat`] and carried in every Decision this module
//! appends, rather than left for somebody to discover.

use aicortex_types::{
    Actor, ActorId, Importance, Memory, MemoryBody, MemoryId, MemoryKind, MemoryParts, Provenance,
    Scope, SourceRef, SourceSystem, Status, TrustClass,
};
use rahi_ledger::{Decision, DecisionId, DecisionKind, Ledger, Outcome};
use rahi_store::{ExecuteResult, Statement, StoreHandle, TxnBuilder, Value};
use rahi_types::{Error, Sub, UnixSeconds};
use serde::Deserialize;

use crate::counters::Counters;
use crate::decision_key::DecisionKeyRepo;
use crate::scope_repo::{ScopeId, seconds_to_sql};

/// The decision kind one erased memory is appended under (B-7).
pub const KIND_ERASE: &str = "memory.erase";

/// The decision kind a completed scope erasure is appended under (B-9).
pub const KIND_ERASE_SCOPE: &str = "memory.erase_scope";

/// The largest number of memories one leased erasure batch will claim (B-9).
///
/// A lease lives [`rahi_store::LEASE_TTL_SECONDS`], and a batch that does not
/// finish inside it is a batch whose holder may have been superseded. The
/// bound also keeps one transaction's statement count to something a store
/// can commit without a long stall on every other writer of the scope.
pub const MAX_ERASURE_BATCH: u32 = 500;

/// The source system a tombstone's stripped provenance names.
const ERASED: &str = "erased";

/// On whose authority a memory is being erased (B-7).
///
/// Carried rather than inferred. The Decision names the subject that asked,
/// and a subject is the only identity this system has: there is no operator
/// back door here and no static key that could stand in for one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Authority {
    subject: Sub,
    reason_code: String,
}

impl Authority {
    /// An erasure asked for by `subject`, with no code beyond the request.
    #[must_use]
    pub fn of(subject: Sub) -> Self {
        Self {
            subject,
            reason_code: "subject_request".to_owned(),
        }
    }

    /// The same, under a named policy: a retention rule, a legal hold lift.
    ///
    /// A short code, never a sentence about the memory: this value reaches
    /// the chain, and a free-text justification is exactly the field somebody
    /// eventually pastes content into.
    #[must_use]
    pub fn because(mut self, reason_code: impl Into<String>) -> Self {
        self.reason_code = reason_code.into();
        self
    }

    /// The subject the Decision names.
    #[must_use]
    pub const fn subject(&self) -> &Sub {
        &self.subject
    }

    /// The policy code the Decision names.
    #[must_use]
    pub fn reason_code(&self) -> &str {
        &self.reason_code
    }
}

/// One table an erasure sweeps, and how a memory is named in it.
///
/// A declared shape rather than a hand-written `DELETE` per table, because
/// "erasure reaches every derivative" is frozen on this file and a sweep
/// written once is a sweep that cannot be half-updated when a table is added.
/// Every entry carries its scope column too: a deletion names its scope in
/// the predicate like every other statement in this crate (012 B-3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Derivative {
    /// The table the rows live in.
    pub table: &'static str,
    /// The column holding the memory's id.
    pub memory_column: &'static str,
    /// The column holding the scope's id.
    pub scope_column: &'static str,
}

impl Derivative {
    /// A derivative table keyed by memory and scope.
    #[must_use]
    pub const fn new(
        table: &'static str,
        memory_column: &'static str,
        scope_column: &'static str,
    ) -> Self {
        Self {
            table,
            memory_column,
            scope_column,
        }
    }
}

/// The derivative tables *this* schema version carries (B-7).
///
/// `decision_key` is not here: its destruction is B-11's own obligation and
/// goes through the two staging calls spec 013 wrote for it
/// ([`DecisionKeyRepo::stage_destroy_for_memory`],
/// [`DecisionKeyRepo::stage_destroy_for_scope`]), which is also where a scope
/// erasure reaches the keys of *refusals*, which stored no row at all.
/// Naming it as an ordinary derivative would hide a frozen invariant inside a
/// loop.
pub const DERIVATIVES: &[Derivative] = &[
    Derivative::new("provenance", "memory_id", "scope_id"),
    Derivative::new("memory_derivation", "memory_id", "scope_id"),
    Derivative::new("memory_source", "memory_id", "scope_id"),
];

/// The derivative tables later specs add, named here before they exist.
///
/// B-7 requires erasure to reach every chunk, every embedding, and every
/// index entry. Those tables belong to specs 015 and 016 and this schema
/// version does not carry them, so sweeping them now would fail every
/// erasure: SQLite refuses a `DELETE` against a table that does not exist and
/// the whole transaction rolls back.
///
/// Naming them anyway is the difference between a requirement deferred and a
/// requirement forgotten. Spec 015 moves its two entries and spec 016 moves
/// its one into [`DERIVATIVES`] in the same change as the migration that
/// creates the table, under an `extends` edge on this file, and the sweep
/// itself needs no edit. Until then [`Eraser::also`] registers them against a
/// table the caller has created, which is how this spec's own tests assert
/// FR-003 over real chunk and embedding rows rather than over their absence.
pub const PLANNED: &[Derivative] = &[
    // Spec 015 section 2: the chunks of a body and the vectors over them.
    Derivative::new("chunk", "memory_id", "scope_id"),
    Derivative::new("embedding", "memory_id", "scope_id"),
    // Spec 016 section 2: the application-owned text index.
    Derivative::new("chunk_token", "memory_id", "scope_id"),
];

/// What one erasure did (B-7, B-8).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Erased {
    /// The memory that is now a tombstone.
    pub memory: MemoryId,
    /// How many derivative rows were removed, across every swept table and
    /// the digest keys.
    pub removed_derivatives: u64,
    /// How many Decision digest keys were destroyed (B-11).
    pub keys_destroyed: u64,
    /// How many memories derived from this one were marked `origin_erased`
    /// rather than erased (B-8).
    pub marked: u64,
    /// The memories erased along with this one, when cascading was asked for.
    pub cascaded: Vec<MemoryId>,
    /// The Decision this erasure appended.
    pub decision: DecisionId,
}

impl Erased {
    /// What an erasure does *not* claim, in so many words (FR-007).
    ///
    /// A backup taken before the erasure still holds the rows it removed,
    /// including the digest keys that make a chained digest confirmable,
    /// until that backup is discarded. Returned as a value rather than left
    /// in a comment because spec 013 FR-008 forbids a surface from implying
    /// otherwise, and the honest sentence has to exist somewhere a surface
    /// can reach for it.
    #[must_use]
    pub const fn caveat() -> &'static str {
        "a backup taken before this erasure still holds the erased rows and their Decision \
         digest keys until that backup is discarded; erasure reaches the live store, not \
         copies already taken from it"
    }
}

/// One erasure, as a value (B-7).
///
/// The parameters are a struct rather than a parameter list because an
/// erasure is the one call in this crate where getting an argument in the
/// wrong position is unrecoverable. `scope` and `memory` are both ids and
/// `cascade` is a bare `bool`; a positional call site that transposed any of
/// them would compile and destroy the wrong thing. Named fields make that a
/// typo the compiler catches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Erasure<'a> {
    /// The scope the memory lives in. Every statement names it (012 B-3).
    pub scope: &'a Scope,
    /// The memory to forget.
    pub memory: MemoryId,
    /// On whose authority, which the Decision names.
    pub authority: &'a Authority,
    /// Whether to erase what was derived from it as well (B-8). Off by
    /// default: a derived summary may be independently valuable, so cascading
    /// is something a caller asks for rather than something it inherits.
    pub cascade: bool,
    /// When. Supplied rather than read from a clock, because this crate reads
    /// no clock: the time a write records is the caller's, and a test that
    /// could not choose it could not assert about it.
    pub at: UnixSeconds,
}

impl<'a> Erasure<'a> {
    /// Erase one memory, leaving what was derived from it standing.
    #[must_use]
    pub const fn new(
        scope: &'a Scope,
        memory: MemoryId,
        authority: &'a Authority,
        at: UnixSeconds,
    ) -> Self {
        Self {
            scope,
            memory,
            authority,
            cascade: false,
            at,
        }
    }

    /// The same, erasing everything derived from it too (B-8).
    #[must_use]
    pub const fn cascading(mut self) -> Self {
        self.cascade = true;
        self
    }
}

/// What a scope erasure did (B-9).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScopeErased {
    /// How many batches it took, across every run that contributed.
    pub batches: u64,
    /// How many memories became tombstones.
    pub memories: u64,
    /// How many derivative rows were removed in total.
    pub removed_derivatives: u64,
    /// How many Decision digest keys were destroyed, including the keys of
    /// refusals that never stored a row (B-11).
    pub keys_destroyed: u64,
    /// The single Decision appended at completion.
    pub decision: DecisionId,
}

/// The erasure of one memory or one scope.
///
/// Holds the sweep, which is the only part of erasure that varies: which
/// tables carry derivatives changes as specs 015 and 016 land, and nothing
/// else about forgetting does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Eraser {
    derivatives: Vec<Derivative>,
}

impl Default for Eraser {
    fn default() -> Self {
        Self {
            derivatives: DERIVATIVES.to_vec(),
        }
    }
}

const TOMBSTONE_SQL: &str = "UPDATE memory
    SET status = 'erased', superseded_by = NULL, fingerprint = '', body_bytes = 0,
        valid_until = NULL, updated = $1, record = $2
    WHERE scope_id = $3 AND id = $4";

/// Mark every memory derived from the erased one (B-8).
///
/// The subquery names the scope too, so a derivative in another scope is not
/// reached: a cross-scope derivation is not a relationship this system
/// exposes (012 B-9).
const MARK_DERIVED_SQL: &str = "UPDATE memory SET origin_erased = 1
    WHERE scope_id = $1 AND origin_erased = 0 AND status <> 'erased' AND id IN (
        SELECT memory_id FROM memory_derivation
        WHERE scope_id = $1 AND parent_id = $2
    )";

const DERIVED_SQL: &str = "SELECT memory_id AS id FROM memory_derivation
    WHERE scope_id = $1 AND parent_id = $2";

const SHELL_SQL: &str = "SELECT id, kind, status, created FROM memory
    WHERE scope_id = $1 AND id = $2 AND status <> 'erased'";

/// The next batch of a scope erasure: whatever is not a tombstone yet (B-9).
///
/// Resumption needs no cursor because the work is defined by what is left. A
/// crash loses at most the batch in flight, and rerunning the pass picks up
/// exactly the memories the crashed batch did not commit.
const BATCH_SQL: &str = "SELECT id, kind, status, created FROM memory
    WHERE scope_id = $1 AND status <> 'erased' ORDER BY id LIMIT $2";

/// One line of per-batch progress (B-9).
///
/// The batch number is computed by the statement for the same reason the
/// source log's ordinal is: a value read before the write could be stale by
/// the time it lands, and a resumed run would then overwrite the progress of
/// the run it is resuming.
const JOURNAL_SQL: &str = "INSERT INTO erasure_journal (
        scope_id, batch, memories, derivatives, at
    ) VALUES (
        $1,
        (SELECT COALESCE(MAX(batch), -1) + 1 FROM erasure_journal WHERE scope_id = $1),
        $2, $3, $4
    )";

const JOURNAL_READ_SQL: &str = "SELECT COUNT(*) AS batches,
        COALESCE(SUM(memories), 0) AS memories,
        COALESCE(SUM(derivatives), 0) AS derivatives
    FROM erasure_journal WHERE scope_id = $1";

#[derive(Debug, Deserialize)]
struct ShellRow {
    id: String,
    kind: String,
    status: String,
    created: i64,
}

#[derive(Debug, Deserialize)]
struct IdRow {
    id: String,
}

#[derive(Debug, Deserialize)]
struct JournalRow {
    batches: i64,
    memories: i64,
    derivatives: i64,
}

impl Eraser {
    /// An eraser sweeping the derivatives this schema version carries.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The same, also sweeping `derivative`.
    ///
    /// For a table this schema version does not carry yet: specs 015 and 016
    /// move their [`PLANNED`] entries into [`DERIVATIVES`] when their
    /// migrations land, and until then a caller that has created the table
    /// itself registers it here. Registering a table that does not exist
    /// fails the erasure rather than silently skipping it, which is the right
    /// way round for an invariant that is frozen.
    #[must_use]
    pub fn also(mut self, derivative: Derivative) -> Self {
        if !self.derivatives.contains(&derivative) {
            self.derivatives.push(derivative);
        }
        self
    }

    /// The tables this eraser sweeps.
    #[must_use]
    pub fn derivatives(&self) -> &[Derivative] {
        &self.derivatives
    }

    /// Erase one memory (B-7, B-8, B-11).
    ///
    /// Every removal is one transaction: the body is replaced by a tombstone,
    /// the derivative rows go, the derivation rows naming this memory as a
    /// child go, its provenance detail and its source log go, and its
    /// Decision digest keys are destroyed, or none of it happens. A partial
    /// erasure is worse than none, because it is an erasure somebody has been
    /// told about.
    ///
    /// The Decision is appended *after* the transaction commits, and that
    /// order is deliberate. The chain is append-only: a Decision appended
    /// first and then not carried out would be a permanent, unretractable
    /// claim that content was destroyed when it was not. The other way round,
    /// a crash between the two loses the record of an erasure that did
    /// happen, which a rerun re-reports and which never misleads anybody
    /// about what the store holds.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when the memory is not a row of this scope or is
    /// already a tombstone; the store's error; the ledger's error when the
    /// Decision cannot be appended.
    pub async fn erase(
        &self,
        store: &StoreHandle,
        ledger: &Ledger,
        request: &Erasure<'_>,
    ) -> Result<Erased, Error> {
        let Erasure {
            scope,
            memory: id,
            authority,
            cascade,
            at: now,
        } = *request;
        let scope_id = ScopeId::of(scope);
        let Some(shell) = self.shell(store, &scope_id, id).await? else {
            return Err(Error::Validation(format!(
                "memory {id} is not an erasable row of this scope"
            )));
        };

        let cascaded = if cascade {
            self.derived_of(store, &scope_id, id).await?
        } else {
            Vec::new()
        };
        let mut shells = vec![shell];
        for derived in &cascaded {
            if let Some(shell) = self.shell(store, &scope_id, *derived).await? {
                shells.push(shell);
            }
        }

        let mut txn = TxnBuilder::new();
        let sweep = self.stage_sweep(&mut txn, &scope_id, &shells);
        // B-11, frozen on this file: the keys of every Decision about this
        // memory are destroyed in the same transaction as the rest of it.
        let keys_at = txn.len();
        for shell in &shells {
            DecisionKeyRepo::stage_destroy_for_memory(&mut txn, scope, shell.id);
        }
        let keys = keys_at..txn.len();
        stage_tombstones(&mut txn, scope, &scope_id, &shells, now)?;
        // B-8: a derivative that is not being erased is marked, never erased.
        // The mark runs after the tombstones, and skips a row that is already
        // one, so a cascaded derivative is reported as erased rather than as
        // merely marked.
        let mark_at = txn.len();
        txn.push(Statement::with_params(
            MARK_DERIVED_SQL,
            vec![Value::from(&scope_id), Value::from(id.to_string())],
        ));

        let results = store.txn(txn.into_statements()).await?;
        let keys_destroyed = span_rows(&results, keys.clone());
        let removed = span_rows(&results, sweep).saturating_add(keys_destroyed);
        let marked = results.get(mark_at).map_or(0, |row| row.rows_affected);

        let decision = DecisionId::new(format!("{KIND_ERASE}-{id}-{}", now.get()));
        ledger
            .append(
                Decision::new(
                    decision.clone(),
                    DecisionKind::new(KIND_ERASE),
                    authority.subject().clone(),
                    Outcome::Allow,
                    "a memory was erased and its derivatives removed",
                )
                .with_payload(serde_json::json!({
                    "scope": scope.to_string(),
                    "memory_id": id.to_string(),
                    "authority": authority.subject().as_str(),
                    "reason_code": authority.reason_code(),
                    "derivatives_removed": removed,
                    "keys_destroyed": keys_destroyed,
                    "derivatives_marked": marked,
                    "cascade": cascade,
                    "cascaded": cascaded.iter().map(ToString::to_string).collect::<Vec<String>>(),
                    "caveat": Erased::caveat(),
                })),
            )
            .await?;

        Ok(Erased {
            memory: id,
            removed_derivatives: removed,
            keys_destroyed,
            marked,
            cascaded,
            decision,
        })
    }

    /// Erase every memory in a scope, in bounded batches under a lease (B-9).
    ///
    /// The same operation as [`Self::erase`] over the whole scope, and the
    /// same transaction boundary: one batch is one commit. Resumable after a
    /// crash without a cursor, because the next batch is whatever is not a
    /// tombstone yet; rerunning after an interruption picks up exactly the
    /// memories the interrupted batch did not commit, and rerunning after
    /// completion erases nothing and says so.
    ///
    /// Every batch destroys the scope's digest keys, not only the keys of the
    /// memories in that batch, and a final destruction runs after the last
    /// batch. `DELETE` is idempotent, so the repetition costs one indexed
    /// statement per batch, and it is what reaches the keys of refusals,
    /// which stored no row at all and are therefore in no batch (013 B-9,
    /// 014 B-11). A scope that holds nothing but refusals still has its keys
    /// destroyed, which is the case a per-memory sweep would miss entirely.
    ///
    /// One Decision at completion, with the per-batch progress in the journal
    /// rather than in the chain: a Decision per batch would put the shape and
    /// the size of somebody's scope into an append-only record.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when `batch` is zero or above
    /// [`MAX_ERASURE_BATCH`]; [`Error::Conflict`] when the lease is
    /// superseded; the store's or the ledger's error.
    pub async fn erase_scope(
        &self,
        store: &StoreHandle,
        ledger: &Ledger,
        scope: &Scope,
        authority: &Authority,
        batch: u32,
        now: UnixSeconds,
    ) -> Result<ScopeErased, Error> {
        if batch == 0 || batch > MAX_ERASURE_BATCH {
            return Err(Error::Validation(format!(
                "an erasure batch of {batch} is not between 1 and {MAX_ERASURE_BATCH}"
            )));
        }
        let scope_id = ScopeId::of(scope);
        let lease = store.lease(&erasure_lease_key(&scope_id)).await?;
        let drained = self.drain_scope(store, scope, &scope_id, batch, now).await;
        lease.release().await;
        let keys_destroyed = drained?;

        let (batches, memories, removed) = self.journal(store, &scope_id).await?;
        let decision = DecisionId::new(format!("{KIND_ERASE_SCOPE}-{scope_id}-{}", now.get()));
        ledger
            .append(
                Decision::new(
                    decision.clone(),
                    DecisionKind::new(KIND_ERASE_SCOPE),
                    authority.subject().clone(),
                    Outcome::Allow,
                    "a scope was erased and its derivatives removed",
                )
                .with_payload(serde_json::json!({
                    "scope": scope.to_string(),
                    "authority": authority.subject().as_str(),
                    "reason_code": authority.reason_code(),
                    "batches": batches,
                    "memories": memories,
                    "derivatives_removed": removed,
                    "keys_destroyed": keys_destroyed,
                    "caveat": Erased::caveat(),
                })),
            )
            .await?;

        Ok(ScopeErased {
            batches,
            memories,
            removed_derivatives: removed,
            keys_destroyed,
            decision,
        })
    }

    /// Erase batch after batch until the scope holds nothing but tombstones.
    ///
    /// Returns how many digest keys were destroyed across the run.
    async fn drain_scope(
        &self,
        store: &StoreHandle,
        scope: &Scope,
        scope_id: &ScopeId,
        batch: u32,
        now: UnixSeconds,
    ) -> Result<u64, Error> {
        let mut keys_destroyed = 0_u64;
        loop {
            let Some(progress) = self
                .drain_one_batch(store, scope, scope_id, batch, now)
                .await?
            else {
                return Ok(keys_destroyed);
            };
            keys_destroyed = keys_destroyed.saturating_add(progress.keys);
            if progress.memories == 0 {
                // The batch read rows that were not erased and then erased
                // none of them. Looping again would read the same rows and do
                // the same nothing, so this stops instead: a pass that cannot
                // make progress is a defect to report, not a spin to hide.
                return Err(Error::Integrity(format!(
                    "the erasure of scope {scope_id} claimed a batch and erased nothing"
                )));
            }
        }
    }

    /// One batch of a scope erasure, or `None` when the scope is drained.
    ///
    /// Called under the caller's lease and inside it: everything a batch
    /// removes is one transaction, and the journal line describing it follows.
    async fn drain_one_batch(
        &self,
        store: &StoreHandle,
        scope: &Scope,
        scope_id: &ScopeId,
        batch: u32,
        now: UnixSeconds,
    ) -> Result<Option<Progress>, Error> {
        {
            let rows: Vec<ShellRow> = store
                .query_consistent(
                    BATCH_SQL,
                    vec![Value::from(scope_id), Value::Integer(i64::from(batch))],
                )
                .await?;
            if rows.is_empty() {
                // The scope holds no live memory. Its keys are still destroyed:
                // a refusal stores no row, so its key is reachable only here,
                // and a scope of nothing but refusals is the case a per-memory
                // sweep would miss entirely.
                let mut txn = TxnBuilder::new();
                DecisionKeyRepo::stage_destroy_for_scope(&mut txn, scope);
                let results = store.txn(txn.into_statements()).await?;
                let _ = span_rows(&results, 0..results.len());
                return Ok(None);
            }
            let shells = rows
                .into_iter()
                .map(Shell::try_from)
                .collect::<Result<Vec<Shell>, Error>>()?;

            let mut txn = TxnBuilder::new();
            let sweep = self.stage_sweep(&mut txn, scope_id, &shells);
            let keys_at = txn.len();
            DecisionKeyRepo::stage_destroy_for_scope(&mut txn, scope);
            let keys = keys_at..txn.len();
            let tombstones = stage_tombstones(&mut txn, scope, scope_id, &shells, now)?;

            let results = store.txn(txn.into_statements()).await?;
            let destroyed = span_rows(&results, keys);
            let removed = span_rows(&results, sweep).saturating_add(destroyed);
            // Counted from the tombstone statements rather than from the rows
            // that were read, so the number is what the transaction did.
            let erased = span_rows(&results, tombstones);

            // The journal is progress, not part of the erasure: it is written
            // after the batch it describes, with the counts the batch actually
            // produced rather than the counts it was expected to produce. A
            // crash between the two loses a line of a report, never a row the
            // erasure was supposed to remove.
            store
                .txn(vec![Statement::with_params(
                    JOURNAL_SQL,
                    vec![
                        Value::from(scope_id),
                        Value::Integer(i64::try_from(erased).unwrap_or(i64::MAX)),
                        Value::Integer(i64::try_from(removed).unwrap_or(i64::MAX)),
                        Value::Integer(seconds_to_sql(now)),
                    ],
                )])
                .await?;
            Ok(Some(Progress {
                memories: erased,
                keys: destroyed,
            }))
        }
    }

    /// The journal's totals for one scope: batches, memories, derivatives.
    async fn journal(
        &self,
        store: &StoreHandle,
        scope: &ScopeId,
    ) -> Result<(u64, u64, u64), Error> {
        let rows: Vec<JournalRow> = store
            .query_consistent(JOURNAL_READ_SQL, vec![Value::from(scope)])
            .await?;
        let row = rows.into_iter().next().unwrap_or(JournalRow {
            batches: 0,
            memories: 0,
            derivatives: 0,
        });
        Ok((
            u64::try_from(row.batches).unwrap_or(0),
            u64::try_from(row.memories).unwrap_or(0),
            u64::try_from(row.derivatives).unwrap_or(0),
        ))
    }

    /// Stage one `DELETE` per swept table per memory, and report their span
    /// in the batch so their row counts can be added up afterwards.
    fn stage_sweep(
        &self,
        txn: &mut TxnBuilder,
        scope: &ScopeId,
        shells: &[Shell],
    ) -> core::ops::Range<usize> {
        let start = txn.len();
        for derivative in &self.derivatives {
            for shell in shells {
                txn.push(Statement::with_params(
                    format!(
                        "DELETE FROM {} WHERE {} = $1 AND {} = $2",
                        derivative.table, derivative.scope_column, derivative.memory_column
                    ),
                    vec![Value::from(scope), Value::from(shell.id.to_string())],
                ));
            }
        }
        start..txn.len()
    }

    /// The columns a tombstone needs, for a memory that is not one yet.
    async fn shell(
        &self,
        store: &StoreHandle,
        scope: &ScopeId,
        id: MemoryId,
    ) -> Result<Option<Shell>, Error> {
        let rows: Vec<ShellRow> = store
            .query_consistent(
                SHELL_SQL,
                vec![Value::from(scope), Value::from(id.to_string())],
            )
            .await?;
        rows.into_iter().next().map(Shell::try_from).transpose()
    }

    /// The memories derived from `id`, in this scope (B-8).
    async fn derived_of(
        &self,
        store: &StoreHandle,
        scope: &ScopeId,
        id: MemoryId,
    ) -> Result<Vec<MemoryId>, Error> {
        let rows: Vec<IdRow> = store
            .query_consistent(
                DERIVED_SQL,
                vec![Value::from(scope), Value::from(id.to_string())],
            )
            .await?;
        rows.into_iter()
            .map(|row| {
                MemoryId::parse(&row.id).map_err(|error| {
                    Error::Integrity(format!(
                        "a derivation row holds an id that is not a memory id: {error}"
                    ))
                })
            })
            .collect()
    }
}

/// What one batch of a scope erasure got through.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Progress {
    /// Memories that became tombstones.
    memories: u64,
    /// Digest keys destroyed.
    keys: u64,
}

/// The lease key a scope's erasure serialises on.
///
/// Per scope rather than global: two subjects' erasures have nothing to say
/// to each other, and one global lease would make forgetting a queue.
#[must_use]
pub fn erasure_lease_key(scope: &ScopeId) -> String {
    format!("aicortex.erasure.{scope}")
}

/// The few columns an erasure needs from a row it is about to empty.
///
/// Read instead of the whole record on purpose: building the tombstone needs
/// the id, the kind and the status (so the counters move off the right
/// bucket) and the creation time, and nothing else. Decoding the body in
/// order to throw it away would put the content through this process for no
/// reason at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Shell {
    id: MemoryId,
    kind: MemoryKind,
    status: Status,
    created: UnixSeconds,
}

impl TryFrom<ShellRow> for Shell {
    type Error = Error;

    fn try_from(row: ShellRow) -> Result<Self, Error> {
        let id = MemoryId::parse(&row.id).map_err(|error| {
            Error::Integrity(format!(
                "memory holds an id that is not a memory id: {error}"
            ))
        })?;
        let kind = MemoryKind::all()
            .into_iter()
            .find(|kind| kind.label() == row.kind)
            .ok_or_else(|| {
                Error::Integrity(format!("memory holds an unknown kind {}", row.kind))
            })?;
        // The status column holds the discriminant, and `Superseded` names a
        // successor the counter bucket does not distinguish. Only the bucket
        // is needed here, and a tombstoned row's successor is cleared anyway.
        let status = match row.status.as_str() {
            "active" => Status::Active,
            "quarantined" => Status::Quarantined,
            "superseded" => Status::Superseded(id),
            "expired" => Status::Expired,
            "erased" => Status::Erased,
            other => {
                return Err(Error::Integrity(format!(
                    "memory holds an unknown status {other}"
                )));
            }
        };
        let created = u64::try_from(row.created)
            .map_err(|_| Error::Integrity("memory holds a negative created time".to_owned()))?;
        Ok(Self {
            id,
            kind,
            status,
            created: UnixSeconds::new(created),
        })
    }
}

/// Stage the tombstone row of every shell, then their counter moves.
///
/// The two are staged in two passes rather than interleaved so that the
/// tombstone statements are a contiguous span of the batch, and the number of
/// memories a batch actually erased is the sum of that span's row counts. A
/// caller that had to know how many statements one shell contributes in order
/// to read its own result would break the first time a counter moved.
///
/// Returns the span the tombstone updates occupy.
fn stage_tombstones(
    txn: &mut TxnBuilder,
    scope: &Scope,
    scope_id: &ScopeId,
    shells: &[Shell],
    now: UnixSeconds,
) -> Result<core::ops::Range<usize>, Error> {
    let start = txn.len();
    for shell in shells {
        let record = serde_json::to_string(&tombstone(scope, shell, now)?).map_err(|error| {
            Error::Validation(format!(
                "the tombstone for {} does not serialize: {error}",
                shell.id
            ))
        })?;
        txn.push(Statement::with_params(
            TOMBSTONE_SQL,
            vec![
                Value::Integer(seconds_to_sql(now)),
                Value::from(record),
                Value::from(scope_id),
                Value::from(shell.id.to_string()),
            ],
        ));
    }
    let span = start..txn.len();
    for shell in shells {
        Counters::adjust(txn, scope_id, shell.kind, &shell.status, -1);
        Counters::adjust(txn, scope_id, shell.kind, &Status::Erased, 1);
    }
    Ok(span)
}

/// The record that replaces an erased memory's own (B-7, D-1).
///
/// Everything that says anything about the subject is gone: the body, the
/// title, the media references, the actor, where it came from, and how much
/// it mattered. What is left is what a dangling reference needs in order to
/// resolve to something honest: the id, the scope, the kind, when it was
/// created, and that it was erased.
///
/// The successor of a superseded row is not carried across. The record is the
/// source of truth for what a memory is (012 D-3) and [`Status::Erased`] has
/// no successor to name; a column that still pointed at one while the record
/// said otherwise would be exactly the disagreement that projection rule
/// exists to prevent.
fn tombstone(scope: &Scope, shell: &Shell, now: UnixSeconds) -> Result<Memory, Error> {
    let source = SourceSystem::new(ERASED)
        .map_err(|error| Error::Integrity(format!("the tombstone source is illegal: {error}")))?;
    let actor = ActorId::new(ERASED)
        .map_err(|error| Error::Integrity(format!("the tombstone actor is illegal: {error}")))?;
    let importance = Importance::new(0.0, shell.created, 0)
        .map_err(|error| Error::Integrity(format!("the tombstone weight is illegal: {error}")))?;
    let mut memory = Memory::new(MemoryParts {
        id: shell.id,
        scope: scope.clone(),
        kind: shell.kind,
        body: MemoryBody::text(""),
        actor: Actor::system(actor),
        provenance: Provenance::captured(SourceRef::new(source), shell.created, shell.created),
        trust: TrustClass::Assertion,
        importance,
        created: shell.created,
    });
    memory.status = Status::Erased;
    memory.updated = now;
    Ok(memory)
}

/// Add up the rows a contiguous span of the batch affected.
fn span_rows(results: &[ExecuteResult], span: core::ops::Range<usize>) -> u64 {
    results
        .get(span)
        .unwrap_or_default()
        .iter()
        .fold(0_u64, |total, row| total.saturating_add(row.rows_affected))
}
