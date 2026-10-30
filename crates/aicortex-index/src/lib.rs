//! The index: similarity computed in this process (spec 016).
//!
//! The chassis refuses loadable extensions (`rahi://016`) and hiqlite's
//! SQLite carries no FTS5, so both retrieval channels are application code
//! over ordinary tables. The vector channel reads stored vectors through
//! rahi's paged read and keeps a bounded top-k heap ([`scan`]); it returns
//! per-channel candidates with scores and owns no ranking policy, which is
//! spec 018's.
//!
//! [`VectorIndex`] is the seam an approximate index may one day fill (B-7);
//! [`ScanIndex`] is its only implementation.

#![forbid(unsafe_code)]
#![warn(missing_docs)]
// Spec 010 B-1: `float_arithmetic` is denied workspace-wide and allowed in
// this crate alone, through this one documented allow at its root. Scoring a
// vector is a dot product and BM25 is a sum of logarithms; neither has an
// integer spelling that is easier to verify than the float one.
#![allow(clippy::float_arithmetic)]

pub mod scan;
pub mod topk;
pub mod vector;

pub use scan::{
    Comparable, DEFAULT_BUDGET, Filter, PageSource, ScanConfig, ScanIndex, ScanOutcome, ScanQuery,
    StoreRows, VectorIndex, VectorRow, scan, scan_rows,
};
pub use topk::{Candidate, TopK};
pub use vector::{VectorError, decode, decode_into, dot, dot_scalar, normalize};
