//! One claim: a subject, a registered predicate, a typed value, the source's
//! stance, and where it came from (spec 050 B-1 to B-4, B-10, B-11).
//!
//! This module is frozen by the `invariant-freeze` constraint on spec 050: a
//! claim cannot be constructed without a subject, a registered predicate
//! reference, a typed value, an epistemic status and provenance. [`Claim`] is
//! `#[non_exhaustive]`, so the only way to one from another crate is
//! [`Claim::new`], which takes a [`ClaimParts`] whose `provenance` field is
//! required; deserialization has no default for it either (FR-003, I-1).
//!
//! The content of a claim is what was said. When it held and when it was
//! recorded are spec 052's, attached around this content rather than inside
//! it. Whether it is admitted, and at what authority, is spec 051's: the
//! [`EpistemicStatus`] here is the source's stance and never aicortex's
//! belief (R-1).

use core::fmt;
use core::str::FromStr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::claim_time::{checked_string, closed_vocabulary, lower_ident};
use crate::claim_value::ClaimValue;
use crate::error::{Result, TypeError, validate_key};
use crate::id::MemoryId;
use crate::predicate::PredicateRef;
use crate::provenance::Provenance;
use crate::scope::Scope;

/// The version every claim written by this code carries.
pub const CLAIM_SCHEMA_VERSION: u16 = 1;

/// The identity of one claim: a UUIDv7 minted by the writer, as a
/// [`MemoryId`] is (B-1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ClaimId(Uuid);

impl ClaimId {
    /// Mint an identifier for a claim being written now.
    #[must_use]
    pub fn now_v7() -> Self {
        Self(Uuid::now_v7())
    }

    /// Carry an identifier minted elsewhere.
    #[must_use]
    pub const fn from_uuid(uuid: Uuid) -> Self {
        Self(uuid)
    }

    /// The identifier as a UUID.
    #[must_use]
    pub const fn as_uuid(&self) -> &Uuid {
        &self.0
    }

    /// Parse a hyphenated UUID.
    ///
    /// # Errors
    ///
    /// [`TypeError::Invalid`] naming `claim_id` when the text is not a UUID.
    pub fn parse(text: &str) -> Result<Self> {
        Uuid::parse_str(text)
            .map(Self)
            .map_err(|error| TypeError::Invalid {
                field: "claim_id",
                reason: format!("{text:?} is not a uuid: {error}"),
            })
    }
}

impl FromStr for ClaimId {
    type Err = TypeError;

    fn from_str(text: &str) -> Result<Self> {
        Self::parse(text)
    }
}

impl fmt::Display for ClaimId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.as_hyphenated())
    }
}

checked_string! {
    /// A vocabulary namespace a domain registers, such as `travel` (B-9).
    ///
    /// The namespace `aicortex` is reserved ([`Namespace::is_reserved`]); a
    /// registry refuses a domain document that claims it.
    Namespace, "namespace", lower_ident
}

impl Namespace {
    /// The namespace reserved for aicortex's own vocabulary (B-9).
    pub const RESERVED: &'static str = "aicortex";

    /// Whether this is the reserved namespace.
    #[must_use]
    pub fn is_reserved(&self) -> bool {
        self.as_str() == Self::RESERVED
    }
}

checked_string! {
    /// A kind of subject a registry declares, such as `segment` (B-3).
    SubjectKind, "subject.kind", lower_ident
}

checked_string! {
    /// The opaque key a domain mints for one subject (B-3).
    SubjectKey, "subject.key", validate_key
}

/// What a claim is about: a namespaced, registered kind and a domain key
/// (B-3).
///
/// A subject is scoped: the same key in two scopes is two subjects. It is not
/// a 017 entity and does not widen 017's closed `EntityKind` (D-7).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubjectRef {
    /// The vocabulary the kind is declared in.
    pub namespace: Namespace,
    /// The kind, as that vocabulary declares it.
    pub kind: SubjectKind,
    /// The domain's key for this subject.
    pub key: SubjectKey,
}

impl fmt::Display for SubjectRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}/{}", self.namespace, self.kind, self.key)
    }
}

checked_string! {
    /// The key that tells apart the values of a predicate with cardinality
    /// `many`, such as a traveler's key for a seat (B-4).
    SlotKey, "slot", validate_key
}

closed_vocabulary! {
    /// The source's stance toward what it says (B-10). Closed.
    ///
    /// This is not aicortex's belief in the statement. Authority is a
    /// separate axis, weighed by spec 051 (R-1).
    Stance, "stance", STANCES {
        /// Stated as so.
        Asserted => "asserted",
        /// Stated as not so.
        Denied => "denied",
        /// Holds if a stated condition holds.
        Conditional => "conditional",
        /// Asked for, such as a seat request.
        Requested => "requested",
        /// Acknowledged by the party able to make it so.
        Confirmed => "confirmed",
        /// An approximate value, such as an estimated arrival.
        Estimated => "estimated",
        /// Scheduled or planned.
        Expected => "expected",
        /// Recorded as having happened, such as a boarding scan.
        Observed => "observed",
    }
}

/// A stance, and for [`Stance::Conditional`] the condition as text (B-10).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "RawEpistemic", into = "RawEpistemic")]
pub struct EpistemicStatus {
    stance: Stance,
    condition: Option<String>,
}

impl EpistemicStatus {
    /// Any stance but [`Stance::Conditional`].
    ///
    /// # Errors
    ///
    /// [`TypeError::MissingField`] naming `condition` for a conditional
    /// stance, which needs [`Self::conditional`].
    pub fn plain(stance: Stance) -> Result<Self> {
        if stance == Stance::Conditional {
            return Err(TypeError::MissingField {
                field: "condition",
                context: "the conditional stance",
            });
        }
        Ok(Self {
            stance,
            condition: None,
        })
    }

    /// Holds if `condition` holds.
    ///
    /// # Errors
    ///
    /// [`TypeError::Invalid`] naming `condition` when it is empty.
    pub fn conditional(condition: impl Into<String>) -> Result<Self> {
        let condition = condition.into();
        if condition.trim().is_empty() {
            return Err(TypeError::Invalid {
                field: "condition",
                reason: "is empty".to_owned(),
            });
        }
        Ok(Self {
            stance: Stance::Conditional,
            condition: Some(condition),
        })
    }

    /// The stance.
    #[must_use]
    pub const fn stance(&self) -> Stance {
        self.stance
    }

    /// The condition, for a conditional stance.
    #[must_use]
    pub fn condition(&self) -> Option<&str> {
        self.condition.as_deref()
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEpistemic {
    stance: Stance,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    condition: Option<String>,
}

impl From<EpistemicStatus> for RawEpistemic {
    fn from(status: EpistemicStatus) -> Self {
        Self {
            stance: status.stance,
            condition: status.condition,
        }
    }
}

impl TryFrom<RawEpistemic> for EpistemicStatus {
    type Error = TypeError;

    fn try_from(raw: RawEpistemic) -> Result<Self> {
        match raw.condition {
            Some(condition) if raw.stance == Stance::Conditional => Self::conditional(condition),
            Some(_) => Err(TypeError::Invalid {
                field: "condition",
                reason: format!("the {} stance carries no condition", raw.stance),
            }),
            None => Self::plain(raw.stance),
        }
    }
}

/// The parts a claim is built from.
///
/// Every field is required and there is no `Default`, so a struct literal is
/// the only builder and it does not compile without a `provenance` (B-2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClaimParts {
    /// The writer-minted identifier.
    pub id: ClaimId,
    /// The scope the claim lives in.
    pub scope: Scope,
    /// What it is about.
    pub subject: SubjectRef,
    /// The registered predicate, at the version it is written under.
    pub predicate: PredicateRef,
    /// What it says.
    pub value: ClaimValue,
    /// The slot, for a predicate with cardinality `many`.
    pub slot: Option<SlotKey>,
    /// The source's stance.
    pub epistemic: EpistemicStatus,
    /// Where it came from.
    pub provenance: Provenance,
}

/// One claim's content (B-1).
///
/// A claim derived from a stored source names that memory in
/// `provenance.derived_from` and points into it with at least one
/// `provenance.spans` entry (B-2, B-12).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct Claim {
    /// The writer-minted identifier.
    pub id: ClaimId,
    /// The scope the claim lives in.
    pub scope: Scope,
    /// What it is about.
    pub subject: SubjectRef,
    /// The registered predicate, at the version it is written under.
    pub predicate: PredicateRef,
    /// What it says.
    pub value: ClaimValue,
    /// The slot, for a predicate with cardinality `many`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot: Option<SlotKey>,
    /// The source's stance.
    pub epistemic: EpistemicStatus,
    /// Where it came from. Required on the wire as in the type.
    pub provenance: Provenance,
    /// The claim record's shape version, [`CLAIM_SCHEMA_VERSION`] when
    /// written by this code.
    pub schema_version: u16,
}

impl Claim {
    /// Build a claim from its parts, stamped with [`CLAIM_SCHEMA_VERSION`].
    ///
    /// ```compile_fail
    /// # use aicortex_types::{Claim, ClaimParts};
    /// # fn build(other: ClaimParts) -> Claim {
    /// // A claim without provenance does not compile (FR-003).
    /// Claim::new(ClaimParts {
    ///     id: other.id,
    ///     scope: other.scope,
    ///     subject: other.subject,
    ///     predicate: other.predicate,
    ///     value: other.value,
    ///     slot: other.slot,
    ///     epistemic: other.epistemic,
    /// })
    /// # }
    /// ```
    #[must_use]
    pub fn new(parts: ClaimParts) -> Self {
        let ClaimParts {
            id,
            scope,
            subject,
            predicate,
            value,
            slot,
            epistemic,
            provenance,
        } = parts;
        Self {
            id,
            scope,
            subject,
            predicate,
            value,
            slot,
            epistemic,
            provenance,
            schema_version: CLAIM_SCHEMA_VERSION,
        }
    }
}

closed_vocabulary! {
    /// How one record relates to another (B-11). Closed.
    RelationKind, "relation", RELATION_KINDS {
        /// The source claim replaces the target; its meaning over time is
        /// spec 052's.
        Supersedes => "supersedes",
        /// The two cannot both hold.
        Contradicts => "contradicts",
        /// The source claim was derived from the target.
        DerivedFrom => "derived_from",
    }
}

closed_vocabulary! {
    /// What a relation points at.
    TargetKind, "target", TARGET_KINDS {
        /// Another claim.
        Claim => "claim",
        /// A stored memory, for [`RelationKind::DerivedFrom`] only.
        Memory => "memory",
    }
}

/// The target of a relation (B-11).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "RawTarget", into = "RawTarget")]
pub enum RelationTarget {
    /// A claim.
    Claim(ClaimId),
    /// A memory.
    Memory(MemoryId),
}

impl RelationTarget {
    /// Which kind of record it points at.
    #[must_use]
    pub const fn kind(&self) -> TargetKind {
        match self {
            Self::Claim(_) => TargetKind::Claim,
            Self::Memory(_) => TargetKind::Memory,
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTarget {
    kind: TargetKind,
    id: Uuid,
}

impl From<RelationTarget> for RawTarget {
    fn from(target: RelationTarget) -> Self {
        let kind = target.kind();
        let id = match target {
            RelationTarget::Claim(id) => *id.as_uuid(),
            RelationTarget::Memory(id) => *id.as_uuid(),
        };
        Self { kind, id }
    }
}

impl TryFrom<RawTarget> for RelationTarget {
    type Error = TypeError;

    fn try_from(raw: RawTarget) -> Result<Self> {
        Ok(match raw.kind {
            TargetKind::Claim => Self::Claim(ClaimId::from_uuid(raw.id)),
            TargetKind::Memory => Self::Memory(MemoryId::from_uuid(raw.id)),
        })
    }
}

/// A relation from one claim to a claim or a memory, appended beside the
/// claim as its own record with its own provenance (B-11).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RawRelation", into = "RawRelation")]
pub struct ClaimRelation {
    from: ClaimId,
    kind: RelationKind,
    to: RelationTarget,
    provenance: Provenance,
}

impl ClaimRelation {
    /// Relate `from` to `to`.
    ///
    /// # Errors
    ///
    /// [`TypeError::Invalid`] naming `target` when a memory is the target of
    /// anything but [`RelationKind::DerivedFrom`], or a claim is related to
    /// itself.
    pub fn new(
        from: ClaimId,
        kind: RelationKind,
        to: RelationTarget,
        provenance: Provenance,
    ) -> Result<Self> {
        if matches!(to, RelationTarget::Memory(_)) && kind != RelationKind::DerivedFrom {
            return Err(TypeError::Invalid {
                field: "target",
                reason: format!("a memory can be the target of derived_from only, not {kind}"),
            });
        }
        if to == RelationTarget::Claim(from) {
            return Err(TypeError::Invalid {
                field: "target",
                reason: "a claim is related to itself".to_owned(),
            });
        }
        Ok(Self {
            from,
            kind,
            to,
            provenance,
        })
    }

    /// The claim the relation is from.
    #[must_use]
    pub const fn from(&self) -> ClaimId {
        self.from
    }

    /// The kind.
    #[must_use]
    pub const fn kind(&self) -> RelationKind {
        self.kind
    }

    /// The target.
    #[must_use]
    pub const fn to(&self) -> RelationTarget {
        self.to
    }

    /// Where the relation came from.
    #[must_use]
    pub const fn provenance(&self) -> &Provenance {
        &self.provenance
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRelation {
    from: ClaimId,
    kind: RelationKind,
    to: RelationTarget,
    provenance: Provenance,
}

impl From<ClaimRelation> for RawRelation {
    fn from(relation: ClaimRelation) -> Self {
        Self {
            from: relation.from,
            kind: relation.kind,
            to: relation.to,
            provenance: relation.provenance,
        }
    }
}

impl TryFrom<RawRelation> for ClaimRelation {
    type Error = TypeError;

    fn try_from(raw: RawRelation) -> Result<Self> {
        Self::new(raw.from, raw.kind, raw.to, raw.provenance)
    }
}
