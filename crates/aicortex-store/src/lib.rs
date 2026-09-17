//! The tables, and the only code allowed to touch them (spec 012).
//!
//! Three of the predecessor's defects were properties of its persistence
//! layer rather than of its ideas, and each is answered here by a mechanism
//! rather than by a convention.
//!
//! - It had **no migrations at all**: the schema was SQL pasted from a
//!   document, so every deployment was a different shape
//!   (`openbrain://no-schema-management`). Here the schema is
//!   [`migrations`], an ordered list of versioned values the chassis applies
//!   through its `migrate` verb, and `serve` refuses to start behind it.
//! - Its **statistics endpoint read every row's metadata** to count things
//!   (`openbrain://stats-loads-everything`). Here every aggregate is a
//!   counter maintained in the writing transaction, and
//!   [`Counters::stats`] is one bounded statement.
//! - Its functions **ran as the database service role** and bypassed row
//!   security, so isolation depended on every call site remembering to
//!   filter (`openbrain://service-role-everywhere`). Here the scope is a
//!   parameter of every call and lands in the statement; there is no method
//!   that reads across scopes and none that takes SQL from a caller.
//!
//! What this crate does not do: decide whether a capture is admissible
//! (013), move a memory between states (014), or own any table a later spec
//! introduces. Those specs add their own migrations to [`migrations`] and
//! their own repositories beside these, each with an `extends` edge on the
//! file it appends to.
//!
//! # Writing
//!
//! A capture is one transaction (constitution XI). The repositories stage
//! statements into the caller's [`rahi_store::TxnBuilder`] and execute
//! nothing:
//!
//! ```no_run
//! # use aicortex_store::MemoryRepo;
//! # use rahi_store::{Envelope, StoreHandle, TxnBuilder};
//! # async fn capture(
//! #     store: &StoreHandle,
//! #     admitted: &aicortex_gate::Admitted,
//! #     work: &Envelope,
//! # ) -> Result<(), rahi_types::Error> {
//! let mut txn = TxnBuilder::new();
//! let provenance = admitted.memory().provenance.clone();
//! MemoryRepo::new().insert(&mut txn, admitted, &provenance, work)?;
//! store.txn(txn.into_statements()).await?;
//! # Ok(())
//! # }
//! ```
//!
//! The memory arrives as an `aicortex_gate::Admitted`, which no code outside
//! the write gate can construct (spec 013 B-1). That is why this crate
//! depends on the gate: the admission boundary is a type, and a type has to
//! be nameable at the seam it guards.
//!
//! # Reading
//!
//! Every read names its consistency and says why (B-5). Admission and
//! uniqueness go through the leader ([`MemoryRepo::fingerprint_holder`],
//! [`ScopeRepo::get`]); listings, details and counts read the local replica.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod counters;
pub mod cursor;
pub mod decision_key;
pub mod memory_repo;
pub mod migrations;
pub mod provenance_repo;
pub mod scope_repo;

pub use counters::{Counters, ScopeStats};
pub use cursor::{Cursor, CursorKey};
pub use decision_key::{DIGEST_ALGORITHM, DecisionKey, DecisionKeyId, DecisionKeyRepo};
pub use memory_repo::{
    DEFAULT_MAX_BODY_BYTES, DEFAULT_PAGE_ROWS, Listing, MAX_PAGE_ROWS, MemoryFilter, MemoryRepo,
    StatusFilter, fingerprint,
};
pub use migrations::{EXPECTED_SCHEMA_VERSION, migrations};
pub use provenance_repo::ProvenanceRepo;
pub use scope_repo::{ScopeId, ScopeRepo, ScopeRow};

/// The lowercase hex SHA-256 of `material`.
///
/// One digest function for the three derived identities this crate mints:
/// the scope id, the content fingerprint, and the filter digest a cursor is
/// signed over. They share a function so they share a hash: a second one
/// added later for convenience is how two parts of a schema end up disagreeing
/// about what a digest of the same bytes is.
#[must_use]
pub(crate) fn hex_digest(material: &[u8]) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, material);
    let mut hex = String::with_capacity(digest.as_ref().len() * 2);
    for byte in digest.as_ref() {
        use core::fmt::Write as _;
        // Writing to a `String` is infallible; the result is discarded
        // rather than unwrapped, which the crate's lints forbid.
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}
