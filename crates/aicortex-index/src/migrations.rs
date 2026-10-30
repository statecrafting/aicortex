//! The text index migration (spec 016 B-5, D-10).
//!
//! The tables are created by `aicortex-store`, below this crate, because
//! erasure lives there and must reach them; this module names the migration
//! from the crate that owns the channel, as spec 015's does for its own.

pub use aicortex_store::text_postings::{TEXT_INDEX_VERSION, text_index_migration};

/// Spec 016's migration: `chunk_token` and `token_df`, at
/// [`TEXT_INDEX_VERSION`]. Not additive (D-10).
#[must_use]
pub fn migration() -> rahi_store::Migration {
    text_index_migration()
}
