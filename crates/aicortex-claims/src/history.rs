//! Append-only claim history, independent of storage and arrival order.

use core::fmt;
use std::collections::{BTreeMap, BTreeSet};

use aicortex_types::{
    Actor, AuthorityLevel, Claim, ClaimId, ClaimRelation, Interval, PredicateRef, ProposalId,
    Provenance, RelationKind, RelationTarget, Scope, SlotKey, Sourcing, SubjectRef,
    SupersessionRule, TimePoint, ValidTimeMode,
};
use rahi_types::UnixSeconds;
use serde::{Deserialize, Serialize};

/// A source-owned revision number.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SourceSeq(pub u64);

/// Transaction time assigned by the store.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TxStamp {
    /// Gap-free sequence within one scope.
    pub seq: u64,
    /// Store wall time for the commit.
    pub recorded_at: UnixSeconds,
}

/// How the claim's validity is represented.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "interval", rename_all = "snake_case")]
pub enum ValidTime {
    /// The source stated an interval.
    Explicit(Box<Interval>),
    /// Valid from the source-declared time until superseded.
    FromSourceTime,
    /// Not limited by valid time.
    Timeless,
}

impl ValidTime {
    /// Whether this representation matches a predicate's registered mode.
    #[must_use]
    pub const fn matches(&self, mode: ValidTimeMode) -> bool {
        matches!(
            (self, mode),
            (Self::Explicit(_), ValidTimeMode::Explicit)
                | (Self::FromSourceTime, ValidTimeMode::FromSourceTime)
                | (Self::Timeless, ValidTimeMode::Timeless)
        )
    }
}

/// The immutable admission identity retained with history.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdmissionRef {
    /// Proposal accepted by the gate.
    pub proposal: ProposalId,
    /// Admission policy id.
    pub policy_id: String,
    /// Admission policy version.
    pub policy_version: u32,
    /// Whether the evidence came from the user, a seed, or a supplier.
    pub sourcing: Sourcing,
}

/// One admitted claim and the three time axes needed to project it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimRecord {
    /// Typed claim content and provenance.
    pub claim: Claim,
    /// Valid-time representation.
    pub valid: ValidTime,
    /// Time declared by the source.
    pub source_time: Option<TimePoint>,
    /// Revision declared by the source.
    pub source_seq: Option<SourceSeq>,
    /// Store-assigned transaction time.
    pub tx: TxStamp,
    /// Authority assigned at admission.
    pub authority: AuthorityLevel,
    /// Immutable admission reference and sourcing class.
    pub admission: AdmissionRef,
    /// Registered supersession behavior frozen at append.
    pub supersession: SupersessionRule,
    /// Classification retained so egress can frame hostile content.
    pub hostile_content: bool,
    /// The source observation was erased without cascading this claim.
    pub origin_erased: bool,
}

impl ClaimRecord {
    /// Validate the time representation against the registered predicate.
    ///
    /// # Errors
    ///
    /// [`HistoryError::InvalidValidTime`] when the mode differs, or a
    /// source-time mode has no source time.
    pub fn validate_time(&self, mode: ValidTimeMode) -> Result<(), HistoryError> {
        if !self.valid.matches(mode)
            || matches!(self.valid, ValidTime::FromSourceTime) && self.source_time.is_none()
        {
            return Err(HistoryError::InvalidValidTime(self.claim.id));
        }
        Ok(())
    }

    /// The slot whose history this record participates in.
    #[must_use]
    pub fn slot_id(&self) -> SlotId {
        SlotId::of(&self.claim)
    }
}

/// A stable slot identity. It deliberately includes scope and subject.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SlotId {
    /// Scope text.
    pub scope: String,
    /// Subject namespace.
    pub subject_namespace: String,
    /// Subject kind.
    pub subject_kind: String,
    /// Subject key.
    pub subject_key: String,
    /// Versioned predicate.
    pub predicate: PredicateRef,
    /// Slot key for cardinality-many predicates.
    pub slot: Option<SlotKey>,
}

impl SlotId {
    /// Build the identity from a claim.
    #[must_use]
    pub fn of(claim: &Claim) -> Self {
        Self {
            scope: claim.scope.to_string(),
            subject_namespace: claim.subject.namespace.to_string(),
            subject_kind: claim.subject.kind.to_string(),
            subject_key: claim.subject.key.to_string(),
            predicate: claim.predicate.clone(),
            slot: claim.slot.clone(),
        }
    }
}

/// An appended relation with the authority and transaction bound that govern it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryRelation {
    /// The typed relation.
    pub relation: ClaimRelation,
    /// Authority at which it was admitted.
    pub authority: AuthorityLevel,
    /// Transaction time of the append.
    pub tx: TxStamp,
}

/// An appended retraction. The target claim remains unchanged.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Retraction {
    /// Claim being retracted.
    pub target: ClaimId,
    /// Stable reason text.
    pub reason: String,
    /// Actor responsible.
    pub by: Actor,
    /// Provenance for the act.
    pub provenance: Provenance,
    /// Transaction time of the append.
    pub tx: TxStamp,
}

/// Refusals while building an in-memory history.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HistoryError {
    /// A claim id was appended twice.
    DuplicateClaim(ClaimId),
    /// A relation names a claim not present in history.
    UnknownClaim(ClaimId),
    /// A supersession crosses slots.
    CrossSlotSupersession,
    /// A transaction sequence is reused in one scope.
    DuplicateTx(u64),
    /// Valid time does not match the predicate mode.
    InvalidValidTime(ClaimId),
}

impl fmt::Display for HistoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateClaim(id) => write!(f, "claim {id} is already in history"),
            Self::UnknownClaim(id) => write!(f, "claim {id} is not in history"),
            Self::CrossSlotSupersession => f.write_str("supersession cannot cross slots"),
            Self::DuplicateTx(seq) => write!(f, "transaction sequence {seq} is already used"),
            Self::InvalidValidTime(id) => write!(f, "claim {id} has invalid valid time"),
        }
    }
}

impl core::error::Error for HistoryError {}

/// Append-only history for one bounded read.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimHistory {
    claims: BTreeMap<ClaimId, ClaimRecord>,
    relations: Vec<HistoryRelation>,
    retractions: Vec<Retraction>,
    used_tx: BTreeMap<String, BTreeSet<u64>>,
}

impl ClaimHistory {
    /// Empty history.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            claims: BTreeMap::new(),
            relations: Vec::new(),
            retractions: Vec::new(),
            used_tx: BTreeMap::new(),
        }
    }

    /// Append a claim without rewriting any earlier record.
    ///
    /// # Errors
    ///
    /// Duplicate claim id or transaction sequence within its scope.
    pub fn append_claim(&mut self, record: ClaimRecord) -> Result<(), HistoryError> {
        if self.claims.contains_key(&record.claim.id) {
            return Err(HistoryError::DuplicateClaim(record.claim.id));
        }
        let scope = record.claim.scope.to_string();
        if !self.used_tx.entry(scope).or_default().insert(record.tx.seq) {
            return Err(HistoryError::DuplicateTx(record.tx.seq));
        }
        self.claims.insert(record.claim.id, record);
        Ok(())
    }

    /// Append a relation, refusing cross-slot supersession.
    ///
    /// # Errors
    ///
    /// An unknown endpoint or a supersession across slots.
    pub fn append_relation(&mut self, relation: HistoryRelation) -> Result<(), HistoryError> {
        let from = relation.relation.from();
        let Some(source) = self.claims.get(&from) else {
            return Err(HistoryError::UnknownClaim(from));
        };
        if let RelationTarget::Claim(target) = relation.relation.to() {
            let Some(target_record) = self.claims.get(&target) else {
                return Err(HistoryError::UnknownClaim(target));
            };
            if relation.relation.kind() == RelationKind::Supersedes
                && source.slot_id() != target_record.slot_id()
            {
                return Err(HistoryError::CrossSlotSupersession);
            }
        }
        self.relations.push(relation);
        Ok(())
    }

    /// Append a retraction without touching its target.
    ///
    /// # Errors
    ///
    /// The target is not in this history.
    pub fn append_retraction(&mut self, retraction: Retraction) -> Result<(), HistoryError> {
        if !self.claims.contains_key(&retraction.target) {
            return Err(HistoryError::UnknownClaim(retraction.target));
        }
        self.retractions.push(retraction);
        Ok(())
    }

    /// Claims by id.
    #[must_use]
    pub const fn claims(&self) -> &BTreeMap<ClaimId, ClaimRecord> {
        &self.claims
    }

    /// Appended relations.
    #[must_use]
    pub fn relations(&self) -> &[HistoryRelation] {
        &self.relations
    }

    /// Appended retractions.
    #[must_use]
    pub fn retractions(&self) -> &[Retraction] {
        &self.retractions
    }

    /// Highest transaction sequence present, or zero for empty history.
    #[must_use]
    pub fn high_water(&self) -> u64 {
        self.claims
            .values()
            .map(|record| record.tx.seq)
            .chain(self.relations.iter().map(|relation| relation.tx.seq))
            .chain(self.retractions.iter().map(|retraction| retraction.tx.seq))
            .max()
            .unwrap_or(0)
    }

    /// Whether all live records belong to the requested scope and subject.
    #[must_use]
    pub fn is_bounded_by(&self, scope: &Scope, subject: &SubjectRef) -> bool {
        self.claims
            .values()
            .all(|record| &record.claim.scope == scope && &record.claim.subject == subject)
    }
}
