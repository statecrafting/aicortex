//! The review queue and promotion (spec 023).
//!
//! Three situations need a person: content whose origin could not be
//! established and was quarantined, two edges that contradict each other, and
//! a memory someone wants a client to act on. [`ReviewQueue`] is the one queue
//! where they wait, [`Reviewer`] applies the decisions that empty it, and
//! [`Promoter`] is the only code that raises a memory to instruction grade
//! (019 B-4, 011 B-5).
//!
//! The queue and the decisions are a library over the store and rahi's
//! ledger. They read no clock (every call takes the time), open no socket,
//! and return memory ids and bare fields, never a body. [`routes`] are the
//! HTTP handlers over them: curation sits above the surfaces, so they use
//! `aicortex-api` downward, the app hands them to its extension seam, and
//! they frame what a person is shown inside 019's envelope (D-2). Every
//! statement names its scope (012 B-3), and nothing here puts memory content
//! into a Decision.

pub mod migrations;
pub mod promotion;
pub mod review;
pub mod routes;

pub use promotion::{
    DEFAULT_REVIEW_INTERVAL_SECS, Demoted, Lapsed, MAX_EXPIRY_BATCH, MAX_REVIEW_INTERVAL_SECS,
    MIN_REVIEW_INTERVAL_SECS, Promoted, Promoter,
};
pub use review::{
    Collected, ContradictionPair, DEFAULT_PAGE_ROWS, MAX_PAGE_ROWS, MAX_REASON_CHARS, Resolution,
    ReviewFilter, ReviewItem, ReviewKind, ReviewPage, ReviewQueue, ReviewState, Reviewer,
};
