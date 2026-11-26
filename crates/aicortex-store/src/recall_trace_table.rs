//! The table a persisted recall trace lives in, and the statement that keeps
//! it true when a scope goes away (spec 018 B-6, D-10).
//!
//! The table lives here, below the recall crate, for the reason the graph's
//! does: scope erasure (spec 014) is in this crate and must reach it, and the
//! crate graph points downward. It is *written* and read by `aicortex-recall`,
//! which owns the trace type; this crate knows only its columns.
//!
//! A row holds ids, scores, and ranks as one JSON document and never a memory
//! body (B-6). It is keyed by the query hash within a scope, and every row
//! carries the instant after which it is no longer read, which is how the
//! retention window of B-6 is enforced.

use rahi_store::{Migration, Statement, TxnBuilder, Value};

/// The version that adds the `recall_trace` table, appended by spec 018 under
/// an `extends` edge on spec 012's migration list.
pub const RECALL_TRACE_VERSION: u32 = crate::migrations::CONVERSATION_VERSION + 1;

const RECALL_TRACE_TABLE: &str = "CREATE TABLE recall_trace (
    trace_id TEXT PRIMARY KEY,
    scope_id TEXT NOT NULL,
    query_hash TEXT NOT NULL,
    recorded_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    trace TEXT NOT NULL,
    delivery TEXT
);
CREATE INDEX recall_trace_scope_hash ON recall_trace (scope_id, query_hash, recorded_at DESC);
CREATE INDEX recall_trace_scope_expiry ON recall_trace (scope_id, expires_at)";

/// The migration of [`RECALL_TRACE_VERSION`].
///
/// Not declared additive: an older binary does not remove a scope's traces
/// when it erases the scope, so serving one across this version could keep the
/// ids of erased memories past their scope.
#[must_use]
pub fn recall_trace_migration() -> Migration {
    Migration::new(
        RECALL_TRACE_VERSION,
        "aicortex persisted recall traces",
        RECALL_TRACE_TABLE,
    )
}

/// Remove every trace of a scope (scope erasure, 014 B-8).
pub fn stage_scope_erasure(txn: &mut TxnBuilder, scope_id: &str) {
    txn.push(Statement::with_params(
        "DELETE FROM recall_trace WHERE scope_id = ?1",
        vec![Value::from(scope_id)],
    ));
}
