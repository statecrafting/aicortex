//! The source adapter framework's tables (spec 030 B-3, B-4, B-8, D-4).
//!
//! The tables live here, below the ingest crate, for the reason spec 017's and
//! 023's do: the migration list is this crate's and the ingest crate depends
//! on it, so the list cannot name a migration the ingest crate owns. They are
//! written and read by `aicortex-ingest`, which owns what a row means; erasure
//! lives here and must reach them (014 B-7, B-8).
//!
//! Nothing here holds memory content. `ingest_item` names a memory by id and
//! carries a digest of the source item as the adapter mapped it, which is what
//! lets a re-run tell an unchanged item from a changed one; because that digest
//! is derived from content, the memory's erasure sweeps the row
//! ([`stage_memory_erasure`]) and a scope's erasure sweeps all three tables.

use rahi_store::{Migration, Statement, TxnBuilder, Value};

/// The source adapter schema of spec 030.
///
/// Version 16: the next free one after 024's decision references (15). The
/// list must be contiguous (053 D-9), so a change that lands first takes the
/// number and the other renumbers this constant and nothing else (030 D-4).
pub const INGEST_VERSION: u32 = 16;

const INGEST_TABLES: &str = "CREATE TABLE ingest_run (
    run_id TEXT PRIMARY KEY,
    scope_id TEXT NOT NULL,
    source_id TEXT NOT NULL,
    started_by TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN
        ('running', 'completed', 'exhausted', 'cancelled', 'failed')),
    cancel_requested INTEGER NOT NULL DEFAULT 0 CHECK (cancel_requested IN (0, 1)),
    item_budget INTEGER NOT NULL CHECK (item_budget > 0),
    rate_per_second INTEGER NOT NULL CHECK (rate_per_second > 0),
    started INTEGER NOT NULL,
    touched INTEGER NOT NULL,
    finished INTEGER,
    seen INTEGER NOT NULL DEFAULT 0,
    admitted INTEGER NOT NULL DEFAULT 0,
    updated INTEGER NOT NULL DEFAULT 0,
    merged INTEGER NOT NULL DEFAULT 0,
    quarantined INTEGER NOT NULL DEFAULT 0,
    refused INTEGER NOT NULL DEFAULT 0,
    unmapped INTEGER NOT NULL DEFAULT 0,
    error_code TEXT,
    CHECK ((state = 'running') = (finished IS NULL)),
    CHECK ((state = 'failed') = (error_code IS NOT NULL))
);
CREATE UNIQUE INDEX ingest_run_active
    ON ingest_run (scope_id, source_id) WHERE state = 'running';
CREATE INDEX ingest_run_latest
    ON ingest_run (scope_id, source_id, started DESC, run_id DESC);
CREATE TABLE ingest_cursor (
    scope_id TEXT NOT NULL,
    source_id TEXT NOT NULL,
    cursor TEXT NOT NULL,
    run_id TEXT NOT NULL,
    touched INTEGER NOT NULL,
    PRIMARY KEY (scope_id, source_id)
);
CREATE TABLE ingest_item (
    scope_id TEXT NOT NULL,
    source_id TEXT NOT NULL,
    external_id TEXT NOT NULL,
    memory_id TEXT NOT NULL,
    digest TEXT NOT NULL,
    touched INTEGER NOT NULL,
    PRIMARY KEY (scope_id, source_id, external_id)
);
CREATE INDEX ingest_item_memory ON ingest_item (scope_id, memory_id)";

/// Spec 030's ingest migration.
///
/// Additive under rahi 036 B-8 (spec 046 B-2): it creates tables and indexes
/// and touches no existing one. An older binary neither reads nor writes
/// them, and one that erases a memory would leave its `ingest_item` row
/// behind, which is why the erasure sweep is part of the binary that creates
/// the tables and not of this migration.
#[must_use]
pub fn ingest_migration() -> Migration {
    Migration::new(INGEST_VERSION, "aicortex source adapters", INGEST_TABLES).additive()
}

/// Remove every ingest row of a scope (scope erasure, 014 B-8).
pub fn stage_scope_erasure(txn: &mut TxnBuilder, scope_id: &str) {
    for table in ["ingest_item", "ingest_cursor", "ingest_run"] {
        txn.push(Statement::with_params(
            format!("DELETE FROM {table} WHERE scope_id = ?1"),
            vec![Value::from(scope_id)],
        ));
    }
}

/// Remove the mappings of `memories`, a batch of one scope's memories being
/// erased (014 B-7).
///
/// One statement for the batch, whatever its size: the per-memory statements
/// of [`crate::DERIVATIVES`] are bounded by the log's write size, and an
/// erasure batch is already at that bound.
pub fn stage_memory_erasure(txn: &mut TxnBuilder, scope_id: &str, memories: &[String]) {
    if memories.is_empty() {
        return;
    }
    let slots = (0..memories.len())
        .map(|index| format!("?{}", index + 2))
        .collect::<Vec<_>>()
        .join(", ");
    let mut params = vec![Value::from(scope_id)];
    params.extend(memories.iter().map(|id| Value::from(id.as_str())));
    txn.push(Statement::with_params(
        format!("DELETE FROM ingest_item WHERE scope_id = ?1 AND memory_id IN ({slots})"),
        params,
    ));
}
