//! The migration the ingest tables arrive in (spec 030 D-4).
//!
//! The tables are created by `aicortex-store`, in its list, because the
//! migration list is the store's and this crate depends on the store and not
//! the other way round; spec 023's curate crate does the same
//! (`aicortex_curate::migrations`). This module names them where the crate
//! that reads them can find them.

pub use aicortex_store::{INGEST_VERSION, ingest_migration};
