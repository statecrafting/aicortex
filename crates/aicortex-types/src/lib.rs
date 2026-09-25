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
//!
//! Spec 050 adds what one typed claim says, on the same terms: [`Claim`]
//! cannot be built without provenance, a registered [`PredicateRef`], a
//! typed [`ClaimValue`] with no floating-point variant, and the source's
//! [`EpistemicStatus`]; the vocabulary is a domain's [`PredicateSet`]; and
//! [`Provenance`] gains [`SourceSpan`]s that point into a stored source by
//! offsets and a keyed digest.
//!
//! Spec 051 adds what an actor offers for a claim: a [`ClaimProposal`], its
//! [`Evidence`], and the closed [`AuthorityLevel`] the gate assigns from it.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod actor;
pub mod claim;
pub mod claim_evidence;
pub mod claim_time;
pub mod claim_value;
pub mod error;
pub mod id;
pub mod memory;
pub mod predicate;
pub mod provenance;
pub mod scope;
pub mod span;
pub mod trust;

pub use actor::{Actor, ActorId, ActorKind, AgentOrigin};
pub use claim::{
    CLAIM_SCHEMA_VERSION, Claim, ClaimId, ClaimParts, ClaimRelation, EpistemicStatus, Namespace,
    RelationKind, RelationTarget, SlotKey, Stance, SubjectKey, SubjectKind, SubjectRef, TargetKind,
};
pub use claim_evidence::{
    AuthorityLevel, ClaimProposal, Evidence, EvidenceKind, ProposalId, ProposedRelation, Score,
    SeedRef, SeedSet, Sourcing,
};
pub use claim_time::{
    Bound, BoundKind, CivilDate, Interval, PointKind, Precision, TimePoint, TimeValue, TzName,
    Uncertainty, Zone, ZoneForm, ZonedTime,
};
pub use claim_value::{
    ClaimValue, CurrencyCode, Decimal, EnumVariant, MAX_SCALE, Unit, UnitKind, UnitName, ValueKind,
};
pub use error::{Result, TypeError};
pub use id::MemoryId;
pub use memory::{
    Importance, MEMORY_SCHEMA_VERSION, MediaDigest, MediaRef, Memory, MemoryBody, MemoryKind,
    MemoryParts, Status,
};
pub use predicate::{
    Cardinality, CardinalityKind, PredicateDef, PredicateDefParts, PredicateName, PredicateRef,
    PredicateSet, SlotName, SupersessionRule, ValidTimeMode, ValueType,
};
pub use provenance::{AdmissionOverride, ExtractorVersion, Provenance, SourceRef, SourceSystem};
pub use scope::{ProjectKey, Scope, ScopeKind, ShareKey};
pub use span::{ContentDigest, Hex, PartKind, PartLocator, SourceSpan, SpanRange, SpanUnit};
pub use trust::{DecisionRef, Promotion, TrustClass};
