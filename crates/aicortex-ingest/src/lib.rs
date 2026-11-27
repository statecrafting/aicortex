//! Source adapters (spec 030).
//!
//! The predecessor's ecosystem grew by forking the server: each capability a
//! copy of the same six hundred lines, so a fix in one never reached the
//! others (`openbrain://fork-per-feature`). This crate is the alternative. A
//! source is an implementation of one trait ([`SourceAdapter`]), registered by
//! name ([`Registry`]), and driven by one runner ([`Runner`]) that owns paging,
//! mapping, the write gate, the transaction, the resumable cursor, rate and
//! budget, and progress. An adapter fetches and maps; it never writes (D-1).
//!
//! What a source inherits without restating it:
//!
//! - **Idempotence.** An item is imported once per source and external id
//!   (`ingest_item`), and a re-run updates what it finds and never duplicates
//!   it; the fingerprint merge of 014 catches the same content arriving from
//!   two sources (B-4).
//! - **Resumability.** A page is one transaction that carries its cursor, so
//!   an interrupted run resumes exactly where it stopped (B-3).
//! - **Provenance.** The runner builds every record's provenance and actor from
//!   the registration, the item, and the run, so an adapter cannot omit or
//!   forge them (B-5).
//! - **The untrusted boundary.** Every candidate goes through the write gate,
//!   whose verdict follows the source's kind (B-7); what a source delivers is
//!   data, bounded and validated, and nothing an adapter returns is trusted.
//! - **Governed egress.** An adapter's hosts are checked against the manifest
//!   ceiling at registration, and every request goes through the kernel's
//!   facade (B-6).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod adapter;
pub mod cursor;
mod drive;
pub mod mapping;
pub mod migrations;
pub mod preflight;
pub mod registry;
pub mod routes;
pub mod run;

pub use adapter::{
    AdapterContext, AdapterError, AdapterResult, BoxFuture, Candidate, MAX_EXTERNAL_ID_BYTES,
    MAX_LOCATOR_BYTES, MAX_OPTIONS_BYTES, MAX_PAGE_ITEMS, Page, Probe, RawItem, SourceAdapter,
    SourceId, SourceKind,
};
pub use cursor::{Cursor, CursorRepo, MAX_CURSOR_BYTES};
pub use mapping::{IngestPolicy, ItemRepo, ItemRow, PublicPolicy, RunId};
pub use registry::{INGEST_SERVICE, Registered, Registry, SourceDescription, ceiling_covers};
pub use run::{
    CancelOutcome, DEFAULT_STALE_AFTER, MAX_BOARD_RUNS, MAX_DRY_RUN_ITEMS, MAX_ITEM_BUDGET,
    MAX_RATE_PER_SECOND, MAX_RETRIES, RunCounts, RunFailure, RunLimits, RunReport, RunRequest,
    RunState, Runner, Runtime, Started,
};
