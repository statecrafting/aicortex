//! The review queue's tables (spec 023 B-1, B-5, B-7, D-3).
//!
//! The tables live here, below the curate crate, for the reason spec 015's,
//! 016's and 017's do: the migration list is this crate's, and the curate
//! crate depends on this one, so the list cannot name a migration the curate
//! crate owns. They are written and read by `aicortex-curate`, which owns what
//! a row means.
//!
//! Nothing here holds memory content. A review item names its subject by id
//! (a memory id, or a contradiction id), and a promotion row names the memory
//! and the ledger Decision that raised it; the body is read from `memory` at
//! the moment a person looks, inside 019's envelope.

use rahi_store::Migration;

/// The review schema of spec 023.
///
/// Version 14: the next free one after 025's conversation reference (12) and
/// 018's recall traces (13). The list must be contiguous (053 D-9), so a
/// change that lands first takes the number and the other renumbers this
/// constant and nothing else (023 D-3).
pub const REVIEW_VERSION: u32 = 14;

const REVIEW_TABLES: &str = "CREATE TABLE review_item (
    item_id TEXT PRIMARY KEY,
    scope_id TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN
        ('quarantine', 'contradiction', 'promotion_request', 'gate_override')),
    subject TEXT NOT NULL,
    created INTEGER NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('open', 'resolved', 'withdrawn')),
    requested_by TEXT,
    resolved_by TEXT,
    resolved_at INTEGER,
    resolution TEXT,
    reason TEXT,
    decision_id TEXT,
    CHECK ((state = 'resolved') = (resolved_by IS NOT NULL))
);
CREATE UNIQUE INDEX review_item_open
    ON review_item (scope_id, kind, subject) WHERE state = 'open';
CREATE INDEX review_item_listing
    ON review_item (scope_id, state, kind, created, item_id);
CREATE TABLE promotion (
    scope_id TEXT NOT NULL,
    memory_id TEXT NOT NULL,
    decision_id TEXT NOT NULL,
    promoted_by TEXT NOT NULL,
    promoted_at INTEGER NOT NULL,
    review_due INTEGER NOT NULL,
    PRIMARY KEY (scope_id, memory_id)
);
CREATE INDEX promotion_due ON promotion (scope_id, review_due)";

/// Spec 023's review migration.
///
/// Additive under rahi 036 B-8 (spec 046 B-2): it creates tables and indexes
/// and touches no existing one, and an older binary neither reads nor writes
/// them. A promotion row is never the only record of an instruction-grade
/// memory (the memory's own record carries the promotion), so an older binary
/// that ignores the table cannot widen what a client may act on.
#[must_use]
pub fn review_migration() -> Migration {
    Migration::new(
        REVIEW_VERSION,
        "aicortex review queue and promotion",
        REVIEW_TABLES,
    )
    .additive()
}
