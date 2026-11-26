//! The graph's tables and the statements that keep them true when a memory
//! goes away (spec 017 B-1 to B-10, D-1).
//!
//! The tables live here, below the graph crate, for the reason spec 015's and
//! 016's do: erasure (spec 014) is in this crate and must reach them, and the
//! crate graph points downward. They are *written* by `aicortex-graph`, which
//! owns resolution and extraction, and *released* here, by every path that
//! erases a memory or a scope.
//!
//! Every graph row is derived. `mention` and `edge_source` are the only rows
//! that name a memory, so they are the whole of what erasure removes per
//! memory (017 FR-006); an edge whose last source is gone is marked
//! `origin_erased` rather than deleted, so the record of what was believed
//! survives without its evidence.

use aicortex_types::MemoryId;
use rahi_store::{Migration, ProcessingKey, ReceiptKey, Statement, TxnBuilder, Value, Work};
use rahi_types::{Error, UnixSeconds};

use crate::embedding_memory::EMBEDDING_NAMESPACE;

/// The graph schema of spec 017, appended under an `extends` edge on spec
/// 012's migration list.
pub const GRAPH_VERSION: u32 = 11;

/// Rahi processor name holding extraction jobs, beside embedding's `embed`
/// in the same namespace (017 B-7).
pub const EXTRACTION_PROCESSOR: &str = "extract";

/// The edge status that records an edge whose last source memory was erased
/// (017 FR-006).
pub const ORIGIN_ERASED: &str = "origin_erased";

const GRAPH_TABLES: &str = "CREATE TABLE entity (
    entity_id TEXT PRIMARY KEY,
    scope_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    canonical TEXT NOT NULL,
    first_seen INTEGER NOT NULL,
    last_seen INTEGER NOT NULL,
    merged_into TEXT,
    merged_at INTEGER,
    vector_model TEXT,
    vector_revision INTEGER,
    vector_dims INTEGER,
    vector_normalized INTEGER,
    vector BLOB
);
CREATE INDEX entity_scope_kind ON entity (scope_id, kind, merged_into);
CREATE TABLE entity_alias (
    scope_id TEXT NOT NULL,
    entity_id TEXT NOT NULL,
    alias TEXT NOT NULL,
    normalized TEXT NOT NULL,
    PRIMARY KEY (scope_id, entity_id, alias)
);
CREATE INDEX entity_alias_lookup ON entity_alias (scope_id, alias);
CREATE INDEX entity_alias_normalized ON entity_alias (scope_id, normalized);
CREATE TABLE entity_merge (
    merge_id TEXT PRIMARY KEY,
    scope_id TEXT NOT NULL,
    survivor TEXT NOT NULL,
    absorbed TEXT NOT NULL,
    merged_at INTEGER NOT NULL,
    retain_until INTEGER NOT NULL,
    reversed_at INTEGER
);
CREATE INDEX entity_merge_scope ON entity_merge (scope_id, absorbed);
CREATE TABLE mention (
    scope_id TEXT NOT NULL,
    memory_id TEXT NOT NULL,
    entity_id TEXT NOT NULL,
    byte_start INTEGER NOT NULL,
    byte_end INTEGER NOT NULL,
    confidence INTEGER NOT NULL CHECK (confidence BETWEEN 0 AND 1000),
    extractor TEXT NOT NULL,
    extractor_version INTEGER NOT NULL,
    PRIMARY KEY (scope_id, memory_id, byte_start, byte_end)
);
CREATE INDEX mention_entity ON mention (scope_id, entity_id);
CREATE TABLE edge (
    edge_id TEXT PRIMARY KEY,
    scope_id TEXT NOT NULL,
    subject TEXT NOT NULL,
    predicate TEXT NOT NULL,
    object_entity TEXT,
    object_literal TEXT,
    confidence INTEGER NOT NULL CHECK (confidence BETWEEN 0 AND 1000),
    extractor TEXT NOT NULL,
    extractor_version INTEGER NOT NULL,
    valid_from INTEGER NOT NULL,
    valid_until INTEGER,
    status TEXT NOT NULL CHECK (status IN ('active', 'superseded', 'origin_erased')),
    created INTEGER NOT NULL,
    CHECK ((object_entity IS NULL) <> (object_literal IS NULL))
);
CREATE UNIQUE INDEX edge_active_fact
    ON edge (scope_id, subject, predicate, COALESCE(object_entity, ''), COALESCE(object_literal, ''))
    WHERE status = 'active';
CREATE INDEX edge_subject ON edge (scope_id, subject, status);
CREATE INDEX edge_object ON edge (scope_id, object_entity, status);
CREATE TABLE edge_source (
    scope_id TEXT NOT NULL,
    edge_id TEXT NOT NULL,
    memory_id TEXT NOT NULL,
    PRIMARY KEY (scope_id, edge_id, memory_id)
);
CREATE INDEX edge_source_memory ON edge_source (scope_id, memory_id);
CREATE TABLE contradiction (
    contradiction_id TEXT PRIMARY KEY,
    scope_id TEXT NOT NULL,
    edge_a TEXT NOT NULL,
    edge_b TEXT NOT NULL,
    subject TEXT NOT NULL,
    predicate TEXT NOT NULL,
    detected INTEGER NOT NULL,
    status TEXT NOT NULL DEFAULT 'open',
    UNIQUE (scope_id, edge_a, edge_b)
);
CREATE TABLE extraction (
    scope_id TEXT NOT NULL,
    memory_id TEXT NOT NULL,
    extractor TEXT NOT NULL,
    extractor_version INTEGER NOT NULL,
    rejected INTEGER NOT NULL DEFAULT 0,
    at INTEGER NOT NULL,
    PRIMARY KEY (scope_id, memory_id, extractor_version)
)";

/// Spec 017's graph migration.
///
/// Not additive (spec 046 B-2): an older binary does not release mentions and
/// edge sources when it erases a memory, so serving one across this version
/// could keep the derivatives of erased content.
#[must_use]
pub fn graph_migration() -> Migration {
    Migration::new(
        GRAPH_VERSION,
        "aicortex entities, aliases, mentions, typed edges",
        GRAPH_TABLES,
    )
}

/// Rahi processor partition for one extractor version.
#[must_use]
pub fn extraction_processor(version: u32) -> String {
    format!("{EXTRACTION_PROCESSOR}.r{version}")
}

/// The durable identity of one memory's extraction at one extractor version
/// (017 B-7): idempotent per `(memory_id, extractor_version)`.
///
/// # Errors
///
/// Rahi validation errors for an invalid scope, memory, or extractor name.
pub fn extraction_key(
    scope_id: &str,
    memory_id: MemoryId,
    extractor: &str,
    version: u32,
) -> Result<ProcessingKey, Error> {
    ProcessingKey::new(
        ReceiptKey::new(scope_id, EMBEDDING_NAMESPACE, memory_id.to_string())?,
        version,
        extraction_processor(version),
        extractor,
    )
}

/// Stage extraction work for one memory into the caller's transaction.
///
/// # Errors
///
/// As [`extraction_key`].
pub fn stage_extraction(
    txn: &mut TxnBuilder,
    scope_id: &str,
    memory_id: MemoryId,
    extractor: &str,
    version: u32,
    now: UnixSeconds,
) -> Result<(), Error> {
    let key = extraction_key(scope_id, memory_id, extractor, version)?;
    Work::stage_work(txn, &key, now);
    Ok(())
}

/// After a sweep: mark every edge left without a source `origin_erased`.
///
/// An edge always has at least one source while it is `active` or
/// `superseded`, so an edge with none lost its last one to erasure. The edge
/// row is kept: neither side of a contradiction is ever deleted (017 FR-005).
pub fn stage_origin_erased(txn: &mut TxnBuilder, scope_id: &str) {
    txn.push(Statement::with_params(
        "UPDATE edge SET status = 'origin_erased'
         WHERE scope_id = ?1 AND status <> 'origin_erased'
           AND NOT EXISTS (
             SELECT 1 FROM edge_source
             WHERE edge_source.scope_id = ?1 AND edge_source.edge_id = edge.edge_id
           )",
        vec![Value::from(scope_id)],
    ));
}

/// Remove every graph row of a scope (scope erasure, 014 B-8).
pub fn stage_scope_erasure(txn: &mut TxnBuilder, scope_id: &str) {
    for table in [
        "mention",
        "edge_source",
        "extraction",
        "contradiction",
        "edge",
        "entity_merge",
        "entity_alias",
        "entity",
    ] {
        txn.push(Statement::with_params(
            format!("DELETE FROM {table} WHERE scope_id = ?1"),
            vec![Value::from(scope_id)],
        ));
    }
}
