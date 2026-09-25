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
#[cfg(test)]
use rahi_store::ExecuteResult;
use rahi_store::{Statement, StoreHandle, TxnBuilder, Value};
use rahi_types::{Error, Sub, UnixSeconds};
use serde::Deserialize;

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

// Accounting adds statements to each row's sweep. Keep the current sweep
// below the pinned engine's 2 MiB WAL entry ceiling even when the caller's
// row ceiling is 500. This is a row bound, not a lease timing assumption.
const MAX_ACCOUNTED_BATCH: u32 = 200;

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
    #[cfg(test)]
    fault: Option<Fault>,
}

impl Default for Eraser {
    fn default() -> Self {
        Self {
            derivatives: DERIVATIVES.to_vec(),
            #[cfg(test)]
            fault: None,
        }
    }
}

const TOMBSTONE_SQL: &str = "UPDATE memory
    SET status = 'erased', superseded_by = NULL, fingerprint = '', body_bytes = 0,
        valid_until = NULL, updated = $1, record = $2
    WHERE scope_id = $3 AND id = $4 AND status <> 'erased'";

/// Move only a row that is still live when this transaction executes. The
/// source bucket comes from the row, not from the earlier shell read: expiry
/// or another erasure may have committed in between. Both counter statements
/// precede the tombstone, inside the same SQLite transaction.
const DECREMENT_SQL: &str = "INSERT INTO scope_counter (scope_id, kind, status, count)
    SELECT scope_id, kind, status, -1 FROM memory
    WHERE scope_id = $1 AND id = $2 AND status <> 'erased'
    ON CONFLICT (scope_id, kind, status)
    DO UPDATE SET count = scope_counter.count - 1";

const INCREMENT_ERASED_SQL: &str = "INSERT INTO scope_counter (scope_id, kind, status, count)
    SELECT scope_id, kind, 'erased', 1 FROM memory
    WHERE scope_id = $1 AND id = $2 AND status <> 'erased'
    ON CONFLICT (scope_id, kind, status)
    DO UPDATE SET count = scope_counter.count + 1";

/// Mark every memory derived from the erased one (B-8).
///
/// The subquery names the scope too, so a derivative in another scope is not
/// reached: a cross-scope derivation is not a relationship this system
/// exposes (012 B-9).
const MARK_DERIVED_SQL: &str = "UPDATE memory SET origin_erased = 1
    WHERE scope_id = ?1 AND origin_erased = 0 AND status <> 'erased' AND id IN (
        SELECT memory_id FROM memory_derivation
        WHERE scope_id = ?1 AND parent_id = ?2
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

/// Totals are scoped to one durable operation, including all resumed runs.
const JOURNAL_READ_SQL: &str = "SELECT COUNT(*) AS batches,
        COALESCE(SUM(memories), 0) AS memories,
        COALESCE(SUM(derivatives), 0) AS derivatives,
        COALESCE(SUM(keys_destroyed), 0) AS keys_destroyed,
        COALESCE(SUM(marked), 0) AS marked
    FROM erasure_journal WHERE scope_id = $1 AND operation = $2";

#[derive(Debug, Deserialize)]
struct Receipt {
    decision_id: String,
    decision: String,
    ready: i64,
    delivered: i64,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fault {
    Journal,
    AfterCommit,
    AfterLedger,
}

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
    keys_destroyed: i64,
    marked: i64,
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
    /// a durable receipt commits with destruction, so a retry delivers the
    /// original Decision without repeating the destructive transaction.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when the memory is not a row of this scope or is
    /// a tombstone without a receipt; the store's error; the ledger's error when the
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
        let target = id.to_string();
        if let Some(receipt) = self.receipt(store, &scope_id, &target).await? {
            return self
                .deliver_memory(store, ledger, &scope_id, &target, receipt)
                .await;
        }
        let Some(shell) = self.shell(store, &scope_id, id).await? else {
            if let Some(receipt) = self.receipt(store, &scope_id, &target).await? {
                return self
                    .deliver_memory(store, ledger, &scope_id, &target, receipt)
                    .await;
            }
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
        let tombstones = stage_tombstones(&mut txn, scope, &scope_id, &shells, now)?;
        // B-8: a derivative that is not being erased is marked, never erased.
        // The mark runs after the tombstones, and skips a row that is already
        // one, so a cascaded derivative is reported as erased rather than as
        // merely marked.
        let mark_at = txn.len();
        txn.push(Statement::with_params(
            MARK_DERIVED_SQL,
            vec![Value::from(&scope_id), Value::from(id.to_string())],
        ));

        let decision = Decision::new(
            DecisionId::new(format!("{KIND_ERASE}-{id}")),
            DecisionKind::new(KIND_ERASE),
            authority.subject().clone(),
            Outcome::Allow,
            "a memory was erased and its derivatives removed",
        )
        .with_payload(serde_json::json!({
            "scope": scope.to_string(), "memory_id": target,
            "authority": authority.subject().as_str(), "reason_code": authority.reason_code(),
            "cascade": cascade,
            "cascaded": cascaded.iter().map(ToString::to_string).collect::<Vec<_>>(),
            "caveat": Erased::caveat(),
        }));
        let mut statements = vec![receipt_insert(&scope_id, &target, &decision)?];
        statements.extend(accounted_batch(
            txn,
            &scope_id,
            decision.id.as_str(),
            now,
            &Counts {
                sweep,
                keys,
                tombstones,
                marked: Some(mark_at),
            },
        ));
        statements.push(Statement::with_params(
            "UPDATE erasure_receipt SET ready = 1 WHERE scope_id = $1 AND target = $2",
            vec![Value::from(&scope_id), Value::from(target.clone())],
        ));
        self.inject_journal_failure(&mut statements);
        if let Err(error) = store.txn(statements).await {
            // A racing request may have committed the same receipt. Only
            // that durable receipt can turn this failure into a safe retry.
            if let Some(receipt) = self.receipt(store, &scope_id, &target).await? {
                return self
                    .deliver_memory(store, ledger, &scope_id, &target, receipt)
                    .await;
            }
            return Err(error);
        }
        self.after_commit()?;
        let receipt = self.required_receipt(store, &scope_id, &target).await?;
        self.deliver_memory(store, ledger, &scope_id, &target, receipt)
            .await
    }

    /// Erase every memory in a scope, in bounded batches under a lease (B-9).
    ///
    /// The same operation as [`Self::erase`] over the whole scope, and the
    /// same transaction boundary: one batch is one commit. Resumable after a
    /// crash without a cursor, because the next batch is whatever is not a
    /// tombstone yet; rerunning after an interruption picks up exactly the
    /// memories the interrupted batch did not commit, and rerunning after
    /// completion returns its original receipt. New captures or keys start a
    /// new operation once the earlier Decision has been delivered.
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
    /// [`MAX_ERASURE_BATCH`]; the store's or the ledger's error.
    ///
    /// The pinned chassis has a stale-lease release defect. Spec 014's dated
    /// Status note records the reproduced TTL-handoff blocker; SQL batch
    /// idempotency does not establish safety of the lock subsystem itself.
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
        let result = self
            .scope_under_lease(store, ledger, scope, authority, batch, now)
            .await;
        lease.release().await;
        result
    }

    async fn scope_under_lease(
        &self,
        store: &StoreHandle,
        ledger: &Ledger,
        scope: &Scope,
        authority: &Authority,
        batch: u32,
        now: UnixSeconds,
    ) -> Result<ScopeErased, Error> {
        let scope_id = ScopeId::of(scope);
        let previous = self.receipt(store, &scope_id, "").await?;
        let has_work: Vec<IdRow> = store
            .query_consistent(
                "SELECT id FROM memory WHERE scope_id = $1 AND status <> 'erased' LIMIT 1",
                vec![Value::from(&scope_id)],
            )
            .await?;
        let keys: Vec<IdRow> = store
            .query_consistent(
                "SELECT key_id AS id FROM decision_key WHERE scope_id = $1 LIMIT 1",
                vec![Value::from(&scope_id)],
            )
            .await?;
        // A completed operation is replayed unless new content or keys arrived.
        // Pending delivery always wins over starting another operation.
        let start = previous
            .as_ref()
            .is_none_or(|r| r.delivered == 1 && (!has_work.is_empty() || !keys.is_empty()));
        if start {
            let legacy: Vec<IdRow> = store
                .query_consistent(
                    "SELECT operation AS id FROM erasure_journal
                 WHERE scope_id = $1 AND operation = 'legacy' LIMIT 1",
                    vec![Value::from(&scope_id)],
                )
                .await?;
            if !legacy.is_empty() {
                return Err(Error::Integrity(
                    "legacy erasure progress has no durable key totals; reconcile it before resuming".into()));
            }
            let decision = Decision::new(
                DecisionId::new(format!("{KIND_ERASE_SCOPE}-{}", MemoryId::now_v7())),
                DecisionKind::new(KIND_ERASE_SCOPE),
                authority.subject().clone(),
                Outcome::Allow,
                "a scope was erased and its derivatives removed",
            )
            .with_payload(serde_json::json!({
                "scope": scope.to_string(), "authority": authority.subject().as_str(),
                "reason_code": authority.reason_code(), "caveat": Erased::caveat(),
            }));
            store.txn(vec![
                Statement::with_params(
                    "DELETE FROM erasure_receipt WHERE scope_id = $1 AND target = '' AND delivered = 1",
                    vec![Value::from(&scope_id)]),
                receipt_insert(&scope_id, "", &decision)?,
            ]).await?;
        }
        let mut receipt = self.required_receipt(store, &scope_id, "").await?;
        if receipt.ready == 0 {
            loop {
                let drained = self
                    .drain_one_batch(store, scope, &receipt.decision_id, batch, now)
                    .await?;
                self.after_commit()?;
                if drained {
                    break;
                }
            }
            receipt = self.required_receipt(store, &scope_id, "").await?;
        }
        let totals = self.journal(store, &scope_id, &receipt.decision_id).await?;
        let decision = self
            .deliver(store, ledger, &scope_id, "", &receipt, &totals)
            .await?;
        Ok(ScopeErased {
            batches: nonnegative(totals.batches),
            memories: nonnegative(totals.memories),
            removed_derivatives: nonnegative(totals.derivatives),
            keys_destroyed: nonnegative(totals.keys_destroyed),
            decision: decision.id,
        })
    }

    /// Destruction, actual-row accounting and completion readiness commit
    /// together. A crash cannot leave a tombstone without its batch report.
    async fn drain_one_batch(
        &self,
        store: &StoreHandle,
        scope: &Scope,
        operation: &str,
        batch: u32,
        now: UnixSeconds,
    ) -> Result<bool, Error> {
        let scope_id = ScopeId::of(scope);
        let rows: Vec<ShellRow> = store
            .query_consistent(
                BATCH_SQL,
                vec![
                    Value::from(&scope_id),
                    Value::Integer(i64::from(batch.min(MAX_ACCOUNTED_BATCH))),
                ],
            )
            .await?;
        let shells = rows
            .into_iter()
            .map(Shell::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        let mut txn = TxnBuilder::new();
        let sweep = self.stage_sweep(&mut txn, &scope_id, &shells);
        let keys_at = txn.len();
        DecisionKeyRepo::stage_destroy_for_scope(&mut txn, scope);
        let keys = keys_at..txn.len();
        let tombstones = stage_tombstones(&mut txn, scope, &scope_id, &shells, now)?;
        let mut statements = vec![Statement::with_params(
            "UPDATE erasure_receipt SET ready = CASE
                WHEN decision_id = $1 AND ready = 0 THEN 0 ELSE NULL END
             WHERE scope_id = $2 AND target = ''",
            vec![Value::from(operation), Value::from(&scope_id)],
        )];
        statements.extend(accounted_batch(
            txn,
            &scope_id,
            operation,
            now,
            &Counts {
                sweep,
                keys,
                tombstones,
                marked: None,
            },
        ));
        // Recheck at commit, including captures arriving after the batch read.
        statements.push(Statement::with_params(
            "UPDATE erasure_receipt SET ready = 1
             WHERE scope_id = ?1 AND target = '' AND decision_id = ?2
             AND NOT EXISTS (SELECT 1 FROM memory WHERE scope_id = ?1 AND status <> 'erased')
             AND NOT EXISTS (SELECT 1 FROM decision_key WHERE scope_id = ?1)",
            vec![Value::from(&scope_id), Value::from(operation)],
        ));
        self.inject_journal_failure(&mut statements);
        let results = store.txn(statements).await?;
        Ok(results.last().is_some_and(|r| r.rows_affected == 1))
    }

    async fn journal(
        &self,
        store: &StoreHandle,
        scope: &ScopeId,
        operation: &str,
    ) -> Result<JournalRow, Error> {
        let rows: Vec<JournalRow> = store
            .query_consistent(
                JOURNAL_READ_SQL,
                vec![Value::from(scope), Value::from(operation)],
            )
            .await?;
        rows.into_iter()
            .next()
            .ok_or_else(|| Error::Integrity("missing erasure totals".into()))
    }

    async fn receipt(
        &self,
        store: &StoreHandle,
        scope: &ScopeId,
        target: &str,
    ) -> Result<Option<Receipt>, Error> {
        let rows: Vec<Receipt> = store
            .query_consistent(
                "SELECT decision_id, decision, ready, delivered FROM erasure_receipt
             WHERE scope_id = $1 AND target = $2",
                vec![Value::from(scope), Value::from(target)],
            )
            .await?;
        Ok(rows.into_iter().next())
    }

    async fn required_receipt(
        &self,
        store: &StoreHandle,
        scope: &ScopeId,
        target: &str,
    ) -> Result<Receipt, Error> {
        self.receipt(store, scope, target)
            .await?
            .ok_or_else(|| Error::Integrity("committed erasure has no durable receipt".into()))
    }

    async fn deliver_memory(
        &self,
        store: &StoreHandle,
        ledger: &Ledger,
        scope: &ScopeId,
        target: &str,
        receipt: Receipt,
    ) -> Result<Erased, Error> {
        let totals = self.journal(store, scope, &receipt.decision_id).await?;
        let decision = self
            .deliver(store, ledger, scope, target, &receipt, &totals)
            .await?;
        let value = serde_json::to_value(&decision).map_err(json_error)?;
        let cascaded_value = value
            .get("payload")
            .and_then(|payload| payload.get("cascaded"))
            .cloned()
            .ok_or_else(|| Error::Integrity("erasure receipt has no cascade list".into()))?;
        let cascaded = serde_json::from_value(cascaded_value).map_err(json_error)?;
        Ok(Erased {
            memory: MemoryId::parse(target).map_err(|e| Error::Integrity(e.to_string()))?,
            removed_derivatives: nonnegative(totals.derivatives),
            keys_destroyed: nonnegative(totals.keys_destroyed),
            marked: nonnegative(totals.marked),
            cascaded,
            decision: decision.id,
        })
    }

    /// The receipt survives both a failed append and an append whose caller
    /// never receives its acknowledgement. Compare the complete Decision,
    /// ignoring only the chain parent the ledger assigns, before accepting
    /// an existing id. Never treat an arbitrary id conflict as success.
    async fn deliver(
        &self,
        store: &StoreHandle,
        ledger: &Ledger,
        scope: &ScopeId,
        target: &str,
        receipt: &Receipt,
        totals: &JournalRow,
    ) -> Result<Decision, Error> {
        if receipt.ready != 1 {
            return Err(Error::Conflict("erasure is still draining".into()));
        }
        let mut value: serde_json::Value =
            serde_json::from_str(&receipt.decision).map_err(json_error)?;
        let payload = value
            .get_mut("payload")
            .and_then(serde_json::Value::as_object_mut)
            .ok_or_else(|| Error::Integrity("erasure receipt payload is not an object".into()))?;
        payload.insert("derivatives_removed".into(), totals.derivatives.into());
        payload.insert("keys_destroyed".into(), totals.keys_destroyed.into());
        if target.is_empty() {
            payload.insert("memories".into(), totals.memories.into());
            payload.insert("batches".into(), totals.batches.into());
        } else {
            payload.insert("derivatives_marked".into(), totals.marked.into());
        }
        let decision: Decision = serde_json::from_value(value).map_err(json_error)?;
        if receipt.delivered == 0
            && !decision_resident(ledger, &decision).await?
            && let Err(error) = ledger.append(decision.clone()).await
            && !decision_resident(ledger, &decision).await?
        {
            return Err(error);
        }
        #[cfg(test)]
        if self.fault == Some(Fault::AfterLedger) {
            return Err(Error::Integrity(
                "injected failure after ledger append".into(),
            ));
        }
        store
            .txn(vec![Statement::with_params(
                "UPDATE erasure_receipt SET delivered = 1
             WHERE scope_id = $1 AND target = $2 AND decision_id = $3",
                vec![
                    Value::from(scope),
                    Value::from(target),
                    Value::from(receipt.decision_id.clone()),
                ],
            )])
            .await?;
        Ok(decision)
    }

    fn inject_journal_failure(&self, _statements: &mut Vec<Statement>) {
        #[cfg(test)]
        if self.fault == Some(Fault::Journal) {
            _statements.push(Statement::new(
                "INSERT INTO missing_erasure_fault_table VALUES (1)",
            ));
        }
    }

    fn after_commit(&self) -> Result<(), Error> {
        #[cfg(test)]
        if self.fault == Some(Fault::AfterCommit) {
            return Err(Error::Integrity(
                "injected failure after destructive commit".into(),
            ));
        }
        Ok(())
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

/// Ranges refer to the original destructive transaction, before accounting
/// statements are interleaved. changes() is captured immediately after each
/// mutation, in the same SQLite transaction, including zero-row overlaps.
struct Counts {
    sweep: core::ops::Range<usize>,
    keys: core::ops::Range<usize>,
    tombstones: core::ops::Range<usize>,
    marked: Option<usize>,
}

fn accounted_batch(
    txn: TxnBuilder,
    scope: &ScopeId,
    operation: &str,
    now: UnixSeconds,
    counts: &Counts,
) -> Vec<Statement> {
    let mut statements = vec![Statement::with_params(
        "INSERT INTO erasure_journal (scope_id, batch, memories, derivatives, at, operation)
         VALUES (?1, (SELECT COALESCE(MAX(batch), -1) + 1 FROM erasure_journal
             WHERE scope_id = ?1), 0, 0, ?2, ?3)",
        vec![
            Value::from(scope),
            Value::Integer(seconds_to_sql(now)),
            Value::from(operation),
        ],
    )];
    for (index, statement) in txn.into_statements().into_iter().enumerate() {
        statements.push(statement);
        let update = if counts.keys.contains(&index) {
            "keys_destroyed = keys_destroyed + changes(), derivatives = derivatives + changes()"
        } else if counts.sweep.contains(&index) {
            "derivatives = derivatives + changes()"
        } else if counts.tombstones.contains(&index) {
            "memories = memories + changes()"
        } else if counts.marked == Some(index) {
            "marked = marked + changes()"
        } else {
            continue;
        };
        statements.push(Statement::with_params(
            format!(
                "UPDATE erasure_journal SET {update} WHERE scope_id = ?1 AND operation = ?2
             AND batch = (SELECT MAX(batch) FROM erasure_journal WHERE scope_id = ?1)"
            ),
            vec![Value::from(scope), Value::from(operation)],
        ));
    }
    statements.push(Statement::with_params(
        "DELETE FROM erasure_journal WHERE scope_id = $1 AND operation = $2
         AND memories = 0 AND derivatives = 0 AND marked = 0",
        vec![Value::from(scope), Value::from(operation)],
    ));
    statements
}

fn receipt_insert(scope: &ScopeId, target: &str, decision: &Decision) -> Result<Statement, Error> {
    Ok(Statement::with_params(
        "INSERT INTO erasure_receipt (scope_id, target, decision_id, decision)
         VALUES (?1,?2,?3, CASE WHEN ?2 = '' OR EXISTS
             (SELECT 1 FROM memory WHERE scope_id = ?1 AND id = ?2 AND status <> 'erased')
             THEN ?4 ELSE NULL END)",
        vec![
            Value::from(scope),
            Value::from(target),
            Value::from(decision.id.as_str()),
            Value::from(serde_json::to_string(decision).map_err(json_error)?),
        ],
    ))
}

async fn decision_resident(ledger: &Ledger, decision: &Decision) -> Result<bool, Error> {
    for record in ledger.records().await? {
        let mut resident = record.decision()?;
        if resident.id == decision.id {
            resident.prev_hash = decision.prev_hash.clone();
            if resident != *decision {
                return Err(Error::Integrity(
                    "erasure Decision id has different content".into(),
                ));
            }
            return Ok(true);
        }
    }
    Ok(false)
}

fn json_error(error: serde_json::Error) -> Error {
    Error::Integrity(error.to_string())
}
fn nonnegative(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
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

/// Stage counter moves from current rows, then their guarded tombstones.
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
    for shell in shells {
        for sql in [DECREMENT_SQL, INCREMENT_ERASED_SQL] {
            txn.push(Statement::with_params(
                sql,
                vec![Value::from(scope_id), Value::from(shell.id.to_string())],
            ));
        }
    }
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
    Ok(start..txn.len())
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
#[cfg(test)]
fn span_rows(results: &[ExecuteResult], span: core::ops::Range<usize>) -> u64 {
    results
        .get(span)
        .unwrap_or_default()
        .iter()
        .fold(0_u64, |total, row| total.saturating_add(row.rows_affected))
}

#[cfg(test)]
#[path = "../tests/common/mod.rs"]
mod test_support;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::{Counters, Lifecycle, MemoryRepo, StatusFilter};
    use test_support as common;

    // Force the read/read/commit/commit interleaving of overlapping erasers.
    // A timing-based concurrent test could pass without ever hitting it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn overlapping_batches_move_each_counter_once_and_keep_the_first_tombstone() {
        let node = common::node().await;
        let alice = common::scope("alice");
        let bob = common::scope("bob");
        let eraser = Eraser::new();
        let scope_id = ScopeId::of(&alice);
        let mut shells = Vec::new();
        for (scope, body) in [(&alice, "first"), (&alice, "second"), (&bob, "other scope")] {
            let memory = common::memory(scope, body, 1_700_000_000);
            let mut txn = TxnBuilder::new();
            MemoryRepo::new()
                .insert(
                    &mut txn,
                    &common::admit(&memory),
                    &memory.provenance,
                    &common::work(&memory),
                )
                .unwrap();
            node.handle().txn(txn.into_statements()).await.unwrap();
            if scope == &alice {
                shells.push(
                    eraser
                        .shell(&node.handle(), &scope_id, memory.id)
                        .await
                        .unwrap()
                        .unwrap(),
                );
            }
        }
        let mut first = TxnBuilder::new();
        let first_span = stage_tombstones(
            &mut first,
            &alice,
            &scope_id,
            &shells,
            UnixSeconds::new(1_700_100_000),
        )
        .unwrap();
        let mut second = TxnBuilder::new();
        let second_span = stage_tombstones(
            &mut second,
            &alice,
            &scope_id,
            &shells,
            UnixSeconds::new(1_700_200_000),
        )
        .unwrap();
        let committed = node.handle().txn(first.into_statements()).await.unwrap();
        assert_eq!(span_rows(&committed, first_span), 2);
        let repeated = node.handle().txn(second.into_statements()).await.unwrap();
        assert_eq!(span_rows(&repeated, second_span), 0);
        let stats = Counters::stats(&node.handle(), &alice).await.unwrap();
        assert_eq!(stats.total, 2);
        assert_eq!(stats.by_status.get(StatusFilter::Erased.label()), Some(&2));
        assert_eq!(stats.by_status.get(StatusFilter::Active.label()), Some(&0));
        let other = Counters::stats(&node.handle(), &bob).await.unwrap();
        assert_eq!(other.total, 1);
        assert_eq!(other.by_status.get(StatusFilter::Active.label()), Some(&1));
        for shell in shells {
            let row = MemoryRepo::new()
                .get(&node.handle(), &alice, shell.id)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(row.updated, UnixSeconds::new(1_700_100_000));
            assert_eq!(row.status, Status::Erased);
        }
        node.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn erasure_moves_the_current_counter_when_expiry_commits_after_the_shell_read() {
        let node = common::node().await;
        let alice = common::scope("alice");
        let scope_id = ScopeId::of(&alice);
        let memory = common::memory(&alice, "expires before erasure commits", 1_700_000_000);
        let now = UnixSeconds::new(1_700_100_000);
        let mut txn = TxnBuilder::new();
        MemoryRepo::new()
            .insert(
                &mut txn,
                &common::admit(&memory),
                &memory.provenance,
                &common::work(&memory),
            )
            .unwrap();
        Lifecycle::set_valid_until(&mut txn, &alice, memory.id, Some(now));
        node.handle().txn(txn.into_statements()).await.unwrap();
        let shell = Eraser::new()
            .shell(&node.handle(), &scope_id, memory.id)
            .await
            .unwrap()
            .unwrap();
        let mut erase = TxnBuilder::new();
        stage_tombstones(&mut erase, &alice, &scope_id, &[shell], now).unwrap();
        assert_eq!(
            Lifecycle::new()
                .expire_due(&node.handle(), &alice, now, 10)
                .await
                .unwrap()
                .memories,
            1
        );
        node.handle().txn(erase.into_statements()).await.unwrap();
        let stats = Counters::stats(&node.handle(), &alice).await.unwrap();
        assert_eq!(stats.total, 1);
        assert_eq!(stats.by_status.get(StatusFilter::Active.label()), Some(&0));
        assert_eq!(stats.by_status.get(StatusFilter::Expired.label()), Some(&0));
        assert_eq!(stats.by_status.get(StatusFilter::Erased.label()), Some(&1));
        node.shutdown().await;
    }
    async fn seed(node: &common::Node, scope: &Scope, number: usize, with_key: bool) -> Memory {
        let memory = common::memory(
            scope,
            &format!("durable erasure fixture {number}"),
            1_700_000_000,
        );
        let mut txn = TxnBuilder::new();
        MemoryRepo::new()
            .insert(
                &mut txn,
                &common::admit(&memory),
                &memory.provenance,
                &common::work(&memory),
            )
            .unwrap();
        if with_key {
            DecisionKeyRepo::stage(
                &mut txn,
                scope,
                &crate::DecisionKey::mint().unwrap(),
                Some(memory.id),
                memory.created,
            );
        }
        node.handle().txn(txn.into_statements()).await.unwrap();
        memory
    }

    async fn block_ledger(node: &common::Node) {
        node.handle()
            .txn(vec![Statement::new(
                "CREATE TRIGGER fail_erasure_ledger BEFORE INSERT ON kernel_decisions
             BEGIN SELECT RAISE(ABORT, 'injected ledger failure'); END",
            )])
            .await
            .unwrap();
    }

    async fn unblock_ledger(node: &common::Node) {
        node.handle()
            .txn(vec![Statement::new(
                "DROP TRIGGER IF EXISTS fail_erasure_ledger",
            )])
            .await
            .unwrap();
    }

    async fn assert_decision(node: &common::Node, id: &DecisionId, keys: u64, removed: u64) {
        let decisions: Vec<_> = node
            .ledger
            .records()
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.decision().unwrap())
            .filter(|d| d.id == *id)
            .collect();
        assert_eq!(decisions.len(), 1);
        let value = serde_json::to_value(&decisions[0]).unwrap();
        assert_eq!(value["payload"]["keys_destroyed"], keys);
        assert_eq!(value["payload"]["derivatives_removed"], removed);
        assert!(!value.to_string().contains("durable erasure fixture"));
        node.ledger.verify().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    // B-7 across sealing and reopen. Under rahi 0.1.0 the retry appended a
    // second copy of the archived Decision (014 Status 2026-09-17); rahi
    // 0.2.0's lifetime-idempotent append (rahi 042) answers the retry with the
    // original record, so exactly one copy exists (014 D-13).
    async fn archived_retry_appends_one_copy_after_reopen() {
        use rahi_ledger::{Archive, Depth, FsArchive, SealPolicy, Segment};

        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            let mut node = common::node().await;
            let archive_dir = tempfile::tempdir().unwrap();
            let archive = FsArchive::open(archive_dir.path()).unwrap();
            let scope = common::scope("alice");
            let memory = seed(&node, &scope, 0, true).await;
            let who = Authority::of(scope.owner.clone());
            let now = UnixSeconds::new(1_700_100_000);
            let eraser = Eraser {
                fault: Some(Fault::AfterLedger),
                ..Eraser::new()
            };
            assert!(
                eraser
                    .erase(
                        &node.handle(),
                        &node.ledger,
                        &Erasure::new(&scope, memory.id, &who, now)
                    )
                    .await
                    .is_err()
            );
            let receipt = eraser
                .required_receipt(&node.handle(), &ScopeId::of(&scope), &memory.id.to_string())
                .await
                .unwrap();
            assert_eq!(receipt.delivered, 0);
            assert_decision(&node, &DecisionId::new(&receipt.decision_id), 1, 2).await;
            node.ledger
                .append(Decision::new(
                    DecisionId::new("archive-fixture-tail"),
                    DecisionKind::new("fixture"),
                    scope.owner.clone(),
                    Outcome::Allow,
                    "keep a resident head",
                ))
                .await
                .unwrap();
            let header = node
                .ledger
                .seal_if_needed(&archive, &SealPolicy::new(2, 2).unwrap())
                .await
                .unwrap()
                .unwrap();
            let segment = Segment::from_bytes(&archive.get(&header.key()).await.unwrap()).unwrap();
            segment.verify().unwrap();
            assert_eq!(segment.header, header);
            assert!(
                segment
                    .records
                    .iter()
                    .any(|r| r.decision().unwrap().id.as_str() == receipt.decision_id)
            );
            node.ledger
                .verify_chain(Depth::Full(&archive))
                .await
                .unwrap();
            node = node.reopen().await;
            let original = segment
                .records
                .iter()
                .map(|r| r.decision().unwrap())
                .find(|d| d.id.as_str() == receipt.decision_id)
                .unwrap();
            let different_authority = Authority::of(common::sub("retrying-operator"));
            assert!(
                !node.ledger.records().await.unwrap().iter().any(|r| r
                    .decision()
                    .unwrap()
                    .id
                    .as_str()
                    == receipt.decision_id)
            );
            let recovered = Eraser::new()
                .erase(
                    &node.handle(),
                    &node.ledger,
                    &Erasure::new(
                        &scope,
                        memory.id,
                        &different_authority,
                        UnixSeconds::new(1_700_200_000),
                    ),
                )
                .await
                .unwrap();
            assert_eq!(recovered.decision.as_str(), receipt.decision_id);
            assert_eq!(recovered.keys_destroyed, 1);
            assert_eq!(recovered.removed_derivatives, 2);
            assert_eq!(original.id, recovered.decision);
            assert!(
                !node
                    .ledger
                    .records()
                    .await
                    .unwrap()
                    .iter()
                    .any(|r| r.decision().unwrap().id == recovered.decision),
                "the retry appended a resident copy of an archived Decision"
            );
            let tombstone = MemoryRepo::new()
                .get(&node.handle(), &scope, memory.id)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(tombstone.status, Status::Erased);
            assert_eq!(tombstone.updated, now);
            assert_eq!(
                eraser
                    .required_receipt(&node.handle(), &ScopeId::of(&scope), &memory.id.to_string())
                    .await
                    .unwrap()
                    .delivered,
                1
            );
            node.ledger
                .verify_chain(Depth::Full(&archive))
                .await
                .unwrap();
            let copies = segment
                .records
                .iter()
                .chain(node.ledger.records().await.unwrap().iter())
                .filter(|r| r.decision().unwrap().id == recovered.decision)
                .count();
            node.shutdown().await;
            assert_eq!(copies, 1, "the retry duplicated an archived Decision");
        })
        .await
        .expect("the archived retry is bounded");
    }

    // Read only in a quiescent fixture. This composes chassis verification
    // APIs for evidence; it is deliberately not an application lookup or a
    // claim that these separate reads form a consistent snapshot.
    async fn diagnostic_history(
        ledger: &Ledger,
        archive: &rahi_ledger::FsArchive,
    ) -> Result<Vec<Decision>, Error> {
        use rahi_ledger::{Archive, Depth, Segment};
        ledger.verify_chain(Depth::Full(archive)).await?;
        let mut decisions = Vec::new();
        for header in ledger.segments().await? {
            let segment = Segment::from_bytes(&archive.get(&header.key()).await?)?;
            segment.verify()?;
            assert_eq!(segment.header, header);
            for record in segment.records {
                decisions.push(record.decision()?);
            }
        }
        for record in ledger.records().await? {
            decisions.push(record.decision()?);
        }
        Ok(decisions)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pinned_archive_failure_is_not_proven_absence() {
        use rahi_ledger::{FsArchive, SealPolicy};
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            let node = common::node().await;
            let archive_dir = tempfile::tempdir().unwrap();
            let archive = FsArchive::open(archive_dir.path()).unwrap();
            let decision = Decision::new(
                DecisionId::new("archived-decision"),
                DecisionKind::new(KIND_ERASE),
                common::sub("alice"),
                Outcome::Allow,
                "diagnostic fixture",
            );
            node.ledger.append(decision.clone()).await.unwrap();
            node.ledger
                .append(Decision::new(
                    DecisionId::new("resident-tail"),
                    DecisionKind::new("fixture"),
                    common::sub("alice"),
                    Outcome::Allow,
                    "keep a resident head",
                ))
                .await
                .unwrap();
            let header = node
                .ledger
                .seal_if_needed(&archive, &SealPolicy::new(2, 2).unwrap())
                .await
                .unwrap()
                .unwrap();
            let mut found = diagnostic_history(&node.ledger, &archive)
                .await
                .unwrap()
                .into_iter()
                .find(|d| d.id == decision.id)
                .unwrap();
            found.prev_hash = decision.prev_hash.clone();
            assert_eq!(
                found, decision,
                "the healthy archive proves complete content"
            );
            assert!(!decision_resident(&node.ledger, &decision).await.unwrap());

            let missing_dir = tempfile::tempdir().unwrap();
            let missing = FsArchive::open(missing_dir.path()).unwrap();
            assert!(
                matches!(
                    diagnostic_history(&node.ledger, &missing).await,
                    Err(Error::NotFound(_))
                ),
                "missing archive data must not mean absent Decision"
            );
            // Damage only this disposable archive file, never chassis tables.
            std::fs::write(archive.root().join(header.key()), b"corrupt archive body").unwrap();
            assert!(
                matches!(
                    diagnostic_history(&node.ledger, &archive).await,
                    Err(Error::Integrity(_))
                ),
                "corrupt archive data must not mean absent Decision"
            );
            let object = archive.root().join(header.key());
            std::fs::remove_file(&object).unwrap();
            std::fs::create_dir(&object).unwrap();
            assert!(
                matches!(
                    diagnostic_history(&node.ledger, &archive).await,
                    Err(Error::Io(_))
                ),
                "unreadable archive data must not mean absent Decision"
            );
            // Resident-only reads cannot distinguish either failure from absence.
            assert!(!decision_resident(&node.ledger, &decision).await.unwrap());
            node.shutdown().await;
        })
        .await
        .expect("archive diagnostic is bounded");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    // B-7 under a concurrent append and seal. Under rahi 0.1.0 the rival
    // append after a sealed first copy produced a second record at another
    // hash; rahi 0.2.0's append is lifetime idempotent, so the rival gets the
    // original hash and history holds one copy (014 D-13).
    async fn concurrent_append_and_seal_keep_one_copy() {
        use rahi_ledger::{FsArchive, SealPolicy};
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            let node = common::node().await;
            let archive_dir = tempfile::tempdir().unwrap();
            let archive = FsArchive::open(archive_dir.path()).unwrap();
            let filler = |id| {
                Decision::new(
                    DecisionId::new(id),
                    DecisionKind::new("fixture"),
                    common::sub("alice"),
                    Outcome::Allow,
                    "keep a resident head",
                )
            };
            node.ledger.append(filler("initial-tail")).await.unwrap();
            node.ledger
                .seal_if_needed(&archive, &SealPolicy::new(1, 1).unwrap())
                .await
                .unwrap()
                .unwrap();
            let decision = Decision::new(
                DecisionId::new("concurrent-erasure"),
                DecisionKind::new(KIND_ERASE),
                common::sub("alice"),
                Outcome::Allow,
                "diagnostic fixture",
            );
            let (looked_up, observed) = tokio::sync::oneshot::channel();
            let (resume, paused) = tokio::sync::oneshot::channel();
            let rival_ledger = node.ledger.clone();
            let rival_archive = archive.clone();
            let rival_decision = decision.clone();
            let rival = tokio::spawn(async move {
                // Even a complete, verified negative read cannot reserve the id.
                assert!(
                    !diagnostic_history(&rival_ledger, &rival_archive)
                        .await
                        .unwrap()
                        .iter()
                        .any(|d| d.id == rival_decision.id)
                );
                looked_up.send(()).unwrap();
                paused.await.unwrap();
                rival_ledger.append(rival_decision).await.unwrap()
            });
            observed.await.unwrap();
            let first_hash = node.ledger.append(decision.clone()).await.unwrap();
            node.ledger.append(filler("final-tail")).await.unwrap();
            node.ledger
                .seal_if_needed(&archive, &SealPolicy::new(2, 2).unwrap())
                .await
                .unwrap()
                .unwrap();
            assert!(!decision_resident(&node.ledger, &decision).await.unwrap());
            resume.send(()).unwrap();
            let second_hash = rival.await.unwrap();
            assert_eq!(
                first_hash, second_hash,
                "the rival append minted a second record"
            );
            let copies: Vec<_> = diagnostic_history(&node.ledger, &archive)
                .await
                .unwrap()
                .into_iter()
                .filter(|d| d.id == decision.id)
                .collect();
            assert_eq!(
                copies.len(),
                1,
                "a concurrent append duplicated the Decision"
            );
            for mut copy in copies {
                copy.prev_hash = decision.prev_hash.clone();
                assert_eq!(copy, decision);
            }
            node.shutdown().await;
        })
        .await
        .expect("the concurrent append is bounded");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn single_receipt_survives_every_commit_and_ledger_boundary() {
        // None means a real ledger INSERT failure, not a simulated return.
        for fault in [
            Some(Fault::Journal),
            Some(Fault::AfterCommit),
            None,
            Some(Fault::AfterLedger),
        ] {
            let mut node = common::node().await;
            let scope = common::scope("alice");
            let memory = seed(&node, &scope, 0, true).await;
            let who = Authority::of(scope.owner.clone());
            let now = UnixSeconds::new(1_700_100_000);
            if fault.is_none() {
                block_ledger(&node).await;
            }
            let eraser = Eraser {
                fault,
                ..Eraser::new()
            };
            assert!(
                eraser
                    .erase(
                        &node.handle(),
                        &node.ledger,
                        &Erasure::new(&scope, memory.id, &who, now)
                    )
                    .await
                    .is_err()
            );
            node = node.reopen().await;
            let row = MemoryRepo::new()
                .get(&node.handle(), &scope, memory.id)
                .await
                .unwrap()
                .unwrap();
            if fault == Some(Fault::Journal) {
                assert_eq!(row, memory, "journal failure rolls back destruction");
                assert!(
                    eraser
                        .receipt(&node.handle(), &ScopeId::of(&scope), &memory.id.to_string())
                        .await
                        .unwrap()
                        .is_none()
                );
                assert_eq!(
                    common::count(
                        &node,
                        "SELECT COUNT(*) AS count FROM decision_key WHERE scope_id = $1",
                        vec![Value::from(&ScopeId::of(&scope))]
                    )
                    .await,
                    1
                );
            } else {
                assert_eq!(row.status, Status::Erased);
                assert_eq!(row.updated, now);
            }
            unblock_ledger(&node).await;
            let retry_at = UnixSeconds::new(1_700_200_000);
            let recovered = Eraser::new()
                .erase(
                    &node.handle(),
                    &node.ledger,
                    &Erasure::new(&scope, memory.id, &who, retry_at),
                )
                .await
                .unwrap();
            assert_eq!(recovered.keys_destroyed, 1);
            assert_eq!(recovered.removed_derivatives, 2);
            assert_decision(&node, &recovered.decision, 1, 2).await;
            node = node.reopen().await;
            let repeated = Eraser::new()
                .erase(
                    &node.handle(),
                    &node.ledger,
                    &Erasure::new(&scope, memory.id, &who, UnixSeconds::new(1_700_300_000)),
                )
                .await
                .unwrap();
            assert_eq!(repeated, recovered);
            let tombstone = MemoryRepo::new()
                .get(&node.handle(), &scope, memory.id)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                tombstone.updated,
                if fault == Some(Fault::Journal) {
                    retry_at
                } else {
                    now
                }
            );
            let stats = Counters::stats(&node.handle(), &scope).await.unwrap();
            assert_eq!(stats.total, 1);
            assert_eq!(stats.by_status.get("erased"), Some(&1));
            assert_decision(&node, &recovered.decision, 1, 2).await;
            node.shutdown().await;
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn scope_receipt_recovers_exact_app_snapshot_totals_at_every_boundary_including_key_only()
    {
        for count in [0, 5] {
            for fault in [
                Some(Fault::Journal),
                Some(Fault::AfterCommit),
                None,
                Some(Fault::AfterLedger),
            ] {
                let mut node = common::node().await;
                let scope = common::scope("alice");
                let bob = common::scope("bob");
                seed(&node, &bob, 99, true).await;
                for number in 0..count {
                    seed(&node, &scope, number, true).await;
                }
                let mut txn = TxnBuilder::new();
                DecisionKeyRepo::stage(
                    &mut txn,
                    &scope,
                    &crate::DecisionKey::mint().unwrap(),
                    None,
                    UnixSeconds::new(1_700_000_000),
                );
                node.handle().txn(txn.into_statements()).await.unwrap();
                let who = Authority::of(scope.owner.clone());
                let now = UnixSeconds::new(1_700_100_000);
                if fault.is_none() {
                    block_ledger(&node).await;
                }
                let eraser = Eraser {
                    fault,
                    ..Eraser::new()
                };
                assert!(
                    eraser
                        .erase_scope(&node.handle(), &node.ledger, &scope, &who, 2, now)
                        .await
                        .is_err()
                );
                node = node.recover_app_snapshot().await;
                let scope_id = ScopeId::of(&scope);
                let receipt = eraser
                    .required_receipt(&node.handle(), &scope_id, "")
                    .await
                    .unwrap();
                let totals = eraser
                    .journal(&node.handle(), &scope_id, &receipt.decision_id)
                    .await
                    .unwrap();
                match fault {
                    Some(Fault::Journal) => {
                        assert_eq!(
                            (totals.batches, totals.memories, totals.keys_destroyed),
                            (0, 0, 0)
                        );
                        assert_eq!(
                            common::count(
                                &node,
                                "SELECT COUNT(*) AS count FROM decision_key WHERE scope_id = $1",
                                vec![Value::from(&scope_id)]
                            )
                            .await,
                            count as u64 + 1
                        );
                    }
                    Some(Fault::AfterCommit) => {
                        assert_eq!(totals.batches, 1);
                        assert_eq!(totals.memories, count.min(2) as i64);
                        assert_eq!(totals.keys_destroyed, count as i64 + 1);
                        assert_eq!(totals.derivatives, (count + count.min(2) + 1) as i64);
                    }
                    _ => assert_eq!(totals.memories, count as i64),
                }
                unblock_ledger(&node).await;
                let result = Eraser::new()
                    .erase_scope(
                        &node.handle(),
                        &node.ledger,
                        &scope,
                        &who,
                        2,
                        UnixSeconds::new(1_700_200_000),
                    )
                    .await
                    .unwrap();
                assert_eq!(result.memories, count as u64);
                assert_eq!(result.keys_destroyed, count as u64 + 1);
                assert_eq!(result.removed_derivatives, count as u64 * 2 + 1);
                assert_eq!(result.batches, if count == 0 { 1 } else { 3 });
                assert_decision(
                    &node,
                    &result.decision,
                    result.keys_destroyed,
                    result.removed_derivatives,
                )
                .await;
                assert_eq!(
                    common::count(
                        &node,
                        "SELECT COUNT(*) AS count FROM decision_key WHERE scope_id = $1",
                        vec![Value::from(&ScopeId::of(&bob))]
                    )
                    .await,
                    1
                );
                node = node.recover_app_snapshot().await;
                let repeated = Eraser::new()
                    .erase_scope(&node.handle(), &node.ledger, &scope, &who, 2, now)
                    .await
                    .unwrap();
                assert_eq!(repeated, result);
                assert_decision(
                    &node,
                    &result.decision,
                    result.keys_destroyed,
                    result.removed_derivatives,
                )
                .await;
                node.shutdown().await;
            }
        }
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn completed_scope_batches_are_fenced_and_new_operations_have_their_own_totals() {
        let mut node = common::node().await;
        let scope = common::scope("alice");
        let who = Authority::of(scope.owner.clone());
        let now = UnixSeconds::new(1_700_100_000);
        seed(&node, &scope, 0, true).await;
        let first = Eraser::new()
            .erase_scope(&node.handle(), &node.ledger, &scope, &who, 2, now)
            .await
            .unwrap();
        node = node.recover_app_snapshot().await;
        let new = seed(&node, &scope, 1, true).await;
        assert!(
            Eraser::new()
                .drain_one_batch(&node.handle(), &scope, first.decision.as_str(), 2, now)
                .await
                .is_err(),
            "a completed operation cannot consume new content"
        );
        assert_eq!(
            MemoryRepo::new()
                .get(&node.handle(), &scope, new.id)
                .await
                .unwrap(),
            Some(new)
        );
        let second = Eraser::new()
            .erase_scope(&node.handle(), &node.ledger, &scope, &who, 2, now)
            .await
            .unwrap();
        assert_ne!(first.decision, second.decision);
        for result in [first, second] {
            assert_eq!(
                (
                    result.batches,
                    result.memories,
                    result.keys_destroyed,
                    result.removed_derivatives
                ),
                (1, 1, 1, 2)
            );
            assert_decision(&node, &result.decision, 1, 2).await;
        }
        node.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn legacy_progress_is_not_reported_as_known_zero_key_destruction() {
        let node = common::node().await;
        let scope = common::scope("alice");
        let memory = seed(&node, &scope, 0, true).await;
        node.handle()
            .txn(vec![Statement::with_params(
                "INSERT INTO erasure_journal (scope_id, batch, memories, derivatives, at)
             VALUES ($1, 0, 1, 2, 1)",
                vec![Value::from(&ScopeId::of(&scope))],
            )])
            .await
            .unwrap();
        let error = Eraser::new()
            .erase_scope(
                &node.handle(),
                &node.ledger,
                &scope,
                &Authority::of(scope.owner.clone()),
                2,
                UnixSeconds::new(2),
            )
            .await
            .unwrap_err();
        assert!(error.message().contains("no durable key totals"));
        assert_eq!(
            MemoryRepo::new()
                .get(&node.handle(), &scope, memory.id)
                .await
                .unwrap(),
            Some(memory)
        );
        assert_eq!(node.ledger.count().await.unwrap(), 1);
        node.shutdown().await;
    }
}
