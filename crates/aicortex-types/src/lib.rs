//! What a memory is, as types, before anything can store one (spec 011).
//!
//! Every crate in this workspace depends on this one, and this one depends on
//! `rahi-types`, serde, and `uuid` and nothing else: no I/O, no SQL, no
//! async, no clock beyond the one [`MemoryId::now_v7`] needs to mint a
//! time-ordered identifier. Spec 011 AC-2 asserts that over the resolved
//! dependency tree rather than trusting the manifest.
//!
//! Three properties are enforced by the type system rather than by a check
//! somebody has to remember to run:
//!
//! - **A memory has provenance or it does not exist.** [`Memory`] is
//!   `#[non_exhaustive]`, so the only way to one from another crate is
//!   [`Memory::new`], which takes a [`MemoryParts`] whose `provenance` field
//!   is required; deserialization has no default for it either.
//! - **An agent cannot promote its own memory.** [`TrustClass::Instruction`]
//!   is a `#[non_exhaustive]` variant reachable only through
//!   [`TrustClass::instruction`], which demands a [`Promotion`], which demands
//!   a human [`rahi_types::Sub`] and the id of a decision in rahi's ledger.
//! - **A model cannot widen a taxonomy.** [`MemoryKind`], [`Status`],
//!   [`TrustClass`], [`ScopeKind`], and [`ActorKind`] are closed; a string
//!   outside one is a [`TypeError`] naming the field, not a passthrough.
//!
//! Nothing here decides admission (spec 013), persists a record (spec 012),
//! or moves one between statuses (spec 014).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod actor;
pub mod error;
pub mod id;
pub mod memory;
pub mod provenance;
pub mod scope;
pub mod trust;

pub use actor::{Actor, ActorId, ActorKind, AgentOrigin};
pub use error::{Result, TypeError};
pub use id::MemoryId;
pub use memory::{
    Importance, MEMORY_SCHEMA_VERSION, MediaDigest, MediaRef, Memory, MemoryBody, MemoryKind,
    MemoryParts, Status,
};
pub use provenance::{AdmissionOverride, ExtractorVersion, Provenance, SourceRef, SourceSystem};
pub use scope::{ProjectKey, Scope, ScopeKind, ShareKey};
pub use trust::{DecisionRef, Promotion, TrustClass};
