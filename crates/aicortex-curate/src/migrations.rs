//! The migration the review queue's tables arrive in (spec 023 D-3).
//!
//! The tables are created by `aicortex-store`, in its list, because the
//! migration list is the store's and this crate depends on the store and not
//! the other way round; spec 017's graph crate does the same
//! (`aicortex_graph::migrations`). This module names them where the crate that
//! reads them can find them.

pub use aicortex_store::{REVIEW_VERSION, review_migration};
