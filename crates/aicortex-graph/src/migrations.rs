//! The graph schema migration (spec 017 B-1, B-4).
//!
//! The tables are created by `aicortex-store`, below this crate, because
//! erasure lives there and must reach them; this module names the migration
//! from the crate that owns the graph, as spec 016's does for its own.

pub use aicortex_store::{GRAPH_VERSION, graph_migration};

/// Spec 017's migration: `entity`, `entity_alias`, `entity_merge`, `mention`,
/// `edge`, `edge_source`, `contradiction`, and `extraction`, at
/// [`GRAPH_VERSION`]. Not additive.
#[must_use]
pub fn migration() -> rahi_store::Migration {
    graph_migration()
}
