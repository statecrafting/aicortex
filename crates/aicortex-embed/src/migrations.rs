//! The embedding schema migrations.

pub use aicortex_store::{EMBEDDING_INTEGRITY_VERSION, EMBEDDING_MIGRATION_VERSION};
use rahi_store::Migration;

/// Spec 015's immutable additive migration.
#[must_use]
pub fn migration() -> Migration {
    aicortex_store::embedding_migration()
}

/// Spec 015's append-only integrity repair.
#[must_use]
pub fn integrity_migration() -> Migration {
    aicortex_store::embedding_integrity_migration()
}
