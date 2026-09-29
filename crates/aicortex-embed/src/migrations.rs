//! The embedding schema migrations.

pub use aicortex_store::EMBEDDING_MIGRATION_VERSION;
use rahi_store::Migration;

/// Spec 015's additive migration.
#[must_use]
pub fn migration() -> Migration {
    aicortex_store::embedding_migration()
}
