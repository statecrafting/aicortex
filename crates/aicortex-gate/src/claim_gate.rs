//! Claim admission (spec 051): the gate's verdict on a proposal, the policy
//! it judges under, and the shape of the Decisions it leads to.
//!
//! Admission is the write gate's second question, asked in the same crate
//! and on the same terms (B-9): [`crate::Gate::evaluate_claim`] is pure, and
//! its output is the only way to an [`AdmittedClaim`], which is the only
//! thing claim history (052) appends.
//!
//! # Order of evaluation
//!
//! 1. The claim against its registered vocabulary (050 B-14):
//!    `UnregisteredPredicate` or `InvalidValue`.
//! 2. The detectors of 013 over every string the claim carries:
//!    `SecretDetected`.
//! 3. Evidence at all: `NoProvenance`.
//! 4. Every stored source it cites, available: `SourceUnavailable`.
//! 5. A seed dressed as the owner's word: `PolicyDenied` (B-14).
//! 6. The score floor: `BelowScoreFloor` (B-6's one use of a score).
//! 7. The authority the evidence earns under the policy's ceilings, none:
//!    `AuthorityInsufficient`.
//! 8. The relations against the claims they target: `PolicyDenied` for a
//!    user's `supersedes` over a supplier claim (B-8, B-10).
//! 9. The ground of admission: a review, an own-data statement, an accepted
//!    seed, or a qualified predicate; otherwise, or when a relation
//!    supersedes a claim of higher authority, `Hold`.
//!
//! No step reads a score except step 6, which is what B-6's perturbation
//! test holds.

use std::collections::BTreeMap;

use aicortex_claims::{ClaimError, RegistrySnapshot};
use aicortex_types::{
    AuthorityLevel, Claim, ClaimId, ClaimProposal, ClaimRelation, DecisionRef, Evidence,
    EvidenceKind, MemoryId, Namespace, PredicateName, ProposalId, RelationKind, RelationTarget,
    Scope, Score, SeedRef, Sourcing,
};
use rahi_types::Sub;
use serde::{Deserialize, Serialize};

use crate::rules::DetectorId;
use crate::verdict::{DigestRef, LedgerEntry};

/// The decision kind of one admission batch (B-11).
pub const KIND_CLAIM_BATCH: &str = "claims.admission.batch";

/// The decision kind of a refused proposal.
pub const KIND_CLAIM_REFUSE: &str = "claims.admission.refuse";

/// The decision kind of a held proposal.
pub const KIND_CLAIM_HOLD: &str = "claims.admission.hold";

/// The decision kind of an admitted user correction.
pub const KIND_CLAIM_CORRECTION: &str = "claims.admission.correction";

/// The decision kind of a new admission policy version.
pub const KIND_POLICY_CHANGE: &str = "claims.policy.change";

/// Which string of a claim a detector fired in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimField {
    /// A `Text` value.
    Value,
    /// A conditional stance's condition.
    Condition,
    /// A `many` predicate's slot key.
    Slot,
    /// The subject's key.
    Subject,
}

/// Why a proposal was refused or held (B-9). Closed, and its own list, so
/// 013 B-2's `Reason` is not amended. Like `Reason`, it carries codes,
/// offsets and identifiers, never a value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum ClaimReason {
    /// No registered version declares the predicate, or it is withdrawn.
    UnregisteredPredicate,
    /// The claim does not validate against its predicate (050 B-14).
    InvalidValue {
        /// 050's refusal code.
        rule: &'static str,
    },
    /// A detector recognised a credential in one of the claim's strings.
    SecretDetected {
        /// Which string.
        field: ClaimField,
        /// Which detector.
        detector: DetectorId,
        /// Where in that string the value starts. The offset, never the
        /// value.
        offset: usize,
    },
    /// A cited stored source is quarantined, erased, or unknown.
    SourceUnavailable {
        /// The source.
        source: MemoryId,
    },
    /// The proposal offers no evidence.
    NoProvenance,
    /// The evidence does not carry enough authority for the policy to admit
    /// it unreviewed, or earns no level at all.
    AuthorityInsufficient,
    /// A model score is below the policy's floor.
    BelowScoreFloor,
    /// A rule of this spec refuses the proposal outright.
    PolicyDenied {
        /// Which rule, by stable name.
        rule: &'static str,
    },
}

impl ClaimReason {
    /// The stable code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::UnregisteredPredicate => "unregistered_predicate",
            Self::InvalidValue { .. } => "invalid_value",
            Self::SecretDetected { .. } => "secret_detected",
            Self::SourceUnavailable { .. } => "source_unavailable",
            Self::NoProvenance => "no_provenance",
            Self::AuthorityInsufficient => "authority_insufficient",
            Self::BelowScoreFloor => "below_score_floor",
            Self::PolicyDenied { .. } => "policy_denied",
        }
    }

    /// The human sentence, which names the shape of the problem and never
    /// the content.
    #[must_use]
    pub fn sentence(&self) -> String {
        match self {
            Self::UnregisteredPredicate => "the predicate is not registered at that version".into(),
            Self::InvalidValue { rule } => format!("the claim breaks its predicate's rule {rule}"),
            Self::SecretDetected {
                field,
                detector,
                offset,
            } => format!(
                "the {detector} detector matched the claim's {field:?} at byte {offset}; a credential is refused"
            ),
            Self::SourceUnavailable { source } => {
                format!("the cited source {source} is quarantined, erased, or unknown")
            }
            Self::NoProvenance => "the proposal offers no evidence".into(),
            Self::AuthorityInsufficient => {
                "the evidence does not carry the authority to admit without review".into()
            }
            Self::BelowScoreFloor => "a model score is below the policy's floor".into(),
            Self::PolicyDenied { rule } => format!("admission rule {rule} refuses the proposal"),
        }
    }
}

impl core::fmt::Display for ClaimReason {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}: {}", self.code(), self.sentence())
    }
}

/// The highest level each kind of evidence can ever earn (B-5). A policy
/// may lower a ceiling and never raise one.
#[must_use]
pub const fn ceiling_of(kind: EvidenceKind) -> Option<AuthorityLevel> {
    match kind {
        EvidenceKind::ModelScore => Some(AuthorityLevel::Inferred),
        EvidenceKind::SourceSpan | EvidenceKind::RuleMatch | EvidenceKind::OperatorSeed => {
            Some(AuthorityLevel::Extracted)
        }
        EvidenceKind::SpanVerified => Some(AuthorityLevel::Verified),
        EvidenceKind::UserStatement => Some(AuthorityLevel::UserCorrected),
        EvidenceKind::Corroboration | EvidenceKind::ReviewApproval => None,
    }
}

/// A policy's ceiling for one kind of evidence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ceiling {
    /// The evidence kind.
    pub evidence: EvidenceKind,
    /// The highest level it earns under this policy.
    pub level: AuthorityLevel,
}

/// A predicate qualified for admission without review (B-12, D-6).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Qualification {
    /// The predicate's namespace.
    pub namespace: Namespace,
    /// The predicate, at any registered version.
    pub predicate: PredicateName,
    /// The least authority a proposal needs to be admitted unreviewed.
    pub min_authority: AuthorityLevel,
    /// The owner decision that qualified it.
    pub decision: DecisionRef,
}

/// A seed set accepted for admission (B-14).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SeedAcceptance {
    /// The set and version, matched exactly.
    pub seed: SeedRef,
    /// The owner decision that accepted it.
    pub decision: DecisionRef,
}

/// Why a policy document is not a legal policy.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum PolicyError {
    /// A ceiling above B-5's for its evidence kind.
    CeilingRaised(EvidenceKind),
    /// Two ceilings for one evidence kind.
    DuplicateCeiling(EvidenceKind),
    /// A qualification that would admit on a model's inference alone (B-6).
    QualifiedOnInference,
    /// A policy with an empty identifier or a version of zero.
    Unnamed,
}

impl core::fmt::Display for PolicyError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::CeilingRaised(kind) => write!(f, "the ceiling for {kind} is above B-5's"),
            Self::DuplicateCeiling(kind) => write!(f, "{kind} has two ceilings"),
            Self::QualifiedOnInference => {
                f.write_str("a qualification below extracted admits on a model's inference")
            }
            Self::Unnamed => f.write_str("a policy needs an id and a version from 1"),
        }
    }
}

impl core::error::Error for PolicyError {}

/// The versioned admission policy (B-12). Data: a new rule is a new
/// version, and an admission record names the version it was judged under.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "PolicyDocument", into = "PolicyDocument")]
pub struct AdmissionPolicy {
    doc: PolicyDocument,
}

/// The wire form of an [`AdmissionPolicy`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyDocument {
    /// The policy's stable name.
    pub id: String,
    /// Its version, from 1.
    pub version: u32,
    /// Refuse a proposal carrying a model score below this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score_floor: Option<Score>,
    /// Per evidence kind, the highest level it earns. A kind not listed
    /// earns B-5's ceiling.
    #[serde(default)]
    pub ceilings: Vec<Ceiling>,
    /// Predicates admitted without review.
    #[serde(default)]
    pub qualified: Vec<Qualification>,
    /// Seed sets admitted without review.
    #[serde(default)]
    pub seeds: Vec<SeedAcceptance>,
}

impl TryFrom<PolicyDocument> for AdmissionPolicy {
    type Error = PolicyError;

    fn try_from(doc: PolicyDocument) -> Result<Self, PolicyError> {
        Self::new(doc)
    }
}

impl From<AdmissionPolicy> for PolicyDocument {
    fn from(policy: AdmissionPolicy) -> Self {
        policy.doc
    }
}

/// The id of the policy [`AdmissionPolicy::initial`] carries.
pub const INITIAL_POLICY_ID: &str = "aicortex.claims.admission";

impl AdmissionPolicy {
    /// Check and adopt a policy document.
    ///
    /// # Errors
    ///
    /// [`PolicyError`] when a ceiling is above B-5's or listed twice, a
    /// qualification is below `extracted`, or the policy is unnamed.
    pub fn new(doc: PolicyDocument) -> Result<Self, PolicyError> {
        if doc.id.trim().is_empty() || doc.version == 0 {
            return Err(PolicyError::Unnamed);
        }
        let mut seen = Vec::with_capacity(doc.ceilings.len());
        for ceiling in &doc.ceilings {
            if seen.contains(&ceiling.evidence) {
                return Err(PolicyError::DuplicateCeiling(ceiling.evidence));
            }
            seen.push(ceiling.evidence);
            match ceiling_of(ceiling.evidence) {
                Some(most) if ceiling.level <= most => {}
                _ => return Err(PolicyError::CeilingRaised(ceiling.evidence)),
            }
        }
        if doc
            .qualified
            .iter()
            .any(|qualification| qualification.min_authority < AuthorityLevel::Extracted)
        {
            return Err(PolicyError::QualifiedOnInference);
        }
        Ok(Self { doc })
    }

    /// The initial policy (B-13, D-6): B-5's ceilings, no score floor, no
    /// qualified predicate, and no accepted seed set.
    #[must_use]
    pub fn initial() -> Self {
        Self {
            doc: PolicyDocument {
                id: INITIAL_POLICY_ID.to_owned(),
                version: 1,
                score_floor: None,
                ceilings: Vec::new(),
                qualified: Vec::new(),
                seeds: Vec::new(),
            },
        }
    }

    /// The document.
    #[must_use]
    pub const fn document(&self) -> &PolicyDocument {
        &self.doc
    }

    /// The policy's name and version.
    #[must_use]
    pub fn reference(&self) -> PolicyRef {
        PolicyRef {
            id: self.doc.id.clone(),
            version: self.doc.version,
        }
    }

    /// The level `kind` earns under this policy.
    #[must_use]
    pub fn ceiling(&self, kind: EvidenceKind) -> Option<AuthorityLevel> {
        let most = ceiling_of(kind)?;
        Some(
            self.doc
                .ceilings
                .iter()
                .find(|ceiling| ceiling.evidence == kind)
                .map_or(most, |ceiling| ceiling.level.min(most)),
        )
    }

    fn qualifies(&self, claim: &Claim, authority: AuthorityLevel) -> bool {
        self.doc.qualified.iter().any(|qualification| {
            qualification.namespace == claim.predicate.namespace
                && qualification.predicate == claim.predicate.name
                && authority >= qualification.min_authority
        })
    }

    fn accepts(&self, seed: &SeedRef) -> bool {
        self.doc.seeds.iter().any(|accepted| &accepted.seed == seed)
    }
}

/// A policy's name and version, as an admission record carries it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PolicyRef {
    /// The policy's stable name.
    pub id: String,
    /// Its version.
    pub version: u32,
}

/// The state of a stored source a proposal cites.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceState {
    /// Stored and visible.
    Available,
    /// Stored out of sight (013 B-7).
    Quarantined,
    /// Erased (014).
    Erased,
}

/// What admission knows about an admitted claim a proposal relates to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetFacts {
    /// The target's scope.
    pub scope: Scope,
    /// The authority it was admitted at.
    pub authority: AuthorityLevel,
    /// Where it came from (B-8).
    pub sourcing: Sourcing,
}

/// Everything the pure gate needs from the store, read by the caller before
/// it asks (the way 013's `Candidate` carries its origin): the state of each
/// stored source the proposal cites, and the facts of each claim it relates
/// to. A source or target the context does not name is treated as
/// unavailable.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimContext {
    /// Cited sources by memory id.
    #[serde(default)]
    pub sources: BTreeMap<MemoryId, SourceState>,
    /// Related claims by claim id.
    #[serde(default)]
    pub targets: BTreeMap<ClaimId, TargetFacts>,
}

/// A proposal the gate accepted (B-3).
///
/// The only way to one is [`ClaimVerdict::Admit`]: the fields are private
/// and the constructor is crate-private, so 052's append taking one is
/// equivalent to "the gate admitted this" (I-2, FR-002).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdmittedClaim {
    claim: Claim,
    proposal: ProposalId,
    proposer: aicortex_types::Actor,
    policy: PolicyRef,
    authority: AuthorityLevel,
    sourcing: Sourcing,
    evidence: Vec<Evidence>,
    relations: Vec<ClaimRelation>,
    corrects: Vec<ClaimId>,
    conflicts: Vec<ClaimId>,
}

impl AdmittedClaim {
    /// The claim, as it will be appended.
    #[must_use]
    pub const fn claim(&self) -> &Claim {
        &self.claim
    }

    /// The proposal it was admitted from.
    #[must_use]
    pub const fn proposal(&self) -> ProposalId {
        self.proposal
    }

    /// Who proposed it.
    #[must_use]
    pub const fn proposer(&self) -> &aicortex_types::Actor {
        &self.proposer
    }

    /// The policy it was judged under.
    #[must_use]
    pub const fn policy(&self) -> &PolicyRef {
        &self.policy
    }

    /// The authority the policy assigned (B-5).
    #[must_use]
    pub const fn authority(&self) -> AuthorityLevel {
        self.authority
    }

    /// Where it came from (B-8).
    #[must_use]
    pub const fn sourcing(&self) -> Sourcing {
        self.sourcing
    }

    /// The evidence considered, whose ids are its positions.
    #[must_use]
    pub fn evidence(&self) -> &[Evidence] {
        &self.evidence
    }

    /// The relations to append with it, carrying the claim's provenance.
    #[must_use]
    pub fn relations(&self) -> &[ClaimRelation] {
        &self.relations
    }

    /// The claims a user correction corrects, empty when it is not one.
    #[must_use]
    pub fn corrects(&self) -> &[ClaimId] {
        &self.corrects
    }

    /// The supplier claims a user correction contradicts: each is a
    /// conflict to surface for review (B-8, 023).
    #[must_use]
    pub fn conflicts(&self) -> &[ClaimId] {
        &self.conflicts
    }

    /// Whether this is an admitted user correction (B-8).
    #[must_use]
    pub fn is_correction(&self) -> bool {
        !self.corrects.is_empty()
    }
}

/// What the gate answers about a proposal (B-9).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClaimVerdict {
    /// Append it.
    Admit(Box<AdmittedClaim>),
    /// Keep it as a proposal until a human reviews it (023).
    Hold(Box<ClaimProposal>, ClaimReason),
    /// Store no claim.
    Refuse(ClaimReason),
}

impl ClaimVerdict {
    /// The reason, when there is one.
    #[must_use]
    pub const fn reason(&self) -> Option<&ClaimReason> {
        match self {
            Self::Admit(_) => None,
            Self::Hold(_, reason) | Self::Refuse(reason) => Some(reason),
        }
    }

    /// `admit`, `hold` or `refuse`.
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Admit(_) => "admit",
            Self::Hold(..) => "hold",
            Self::Refuse(_) => "refuse",
        }
    }

    /// The admitted claim, when there is one.
    #[must_use]
    pub fn admitted(&self) -> Option<&AdmittedClaim> {
        match self {
            Self::Admit(admitted) => Some(admitted),
            _ => None,
        }
    }
}

/// Map 050's refusal to this spec's reason.
fn invalid(error: &ClaimError) -> ClaimReason {
    match error {
        ClaimError::UnknownPredicate(_)
        | ClaimError::WrongVersion(_)
        | ClaimError::Withdrawn(_) => ClaimReason::UnregisteredPredicate,
        other => ClaimReason::InvalidValue { rule: other.code() },
    }
}

impl crate::Gate {
    /// Judge a claim proposal (B-9).
    ///
    /// Pure: no clock, no randomness, no network, no store read. The store's
    /// facts arrive in `context`, read by the caller, so the same proposal,
    /// registry, policy and context always yield the same verdict (R-1).
    #[must_use]
    pub fn evaluate_claim(
        &self,
        proposal: &ClaimProposal,
        registry: &RegistrySnapshot,
        policy: &AdmissionPolicy,
        context: &ClaimContext,
    ) -> ClaimVerdict {
        match self.judge(proposal, registry, policy, context) {
            Judgement::Refuse(reason) => ClaimVerdict::Refuse(reason),
            Judgement::Hold(reason) => ClaimVerdict::Hold(Box::new(proposal.clone()), reason),
            Judgement::Admit(admitted) => ClaimVerdict::Admit(admitted),
        }
    }

    /// The material a refusal Decision's keyed digest is taken over (013
    /// B-9): the scope, the predicate, and the value's wire form. Like
    /// [`crate::Gate::digest_material`], its one legitimate destination is
    /// the keyed digest.
    #[must_use]
    pub fn claim_digest_material(&self, proposal: &ClaimProposal) -> Vec<u8> {
        let _ = self;
        let claim = &proposal.claim;
        let value = serde_json::to_vec(&claim.value).unwrap_or_default();
        let mut material = Vec::with_capacity(value.len() + 96);
        material.extend_from_slice(claim.scope.to_string().as_bytes());
        material.push(0x1f);
        material.extend_from_slice(claim.predicate.to_string().as_bytes());
        material.push(0x1f);
        material.extend_from_slice(&value);
        material
    }

    fn judge(
        &self,
        proposal: &ClaimProposal,
        registry: &RegistrySnapshot,
        policy: &AdmissionPolicy,
        context: &ClaimContext,
    ) -> Judgement {
        let claim = &proposal.claim;
        if let Err(error) = aicortex_claims::validate(claim, registry) {
            return Judgement::Refuse(invalid(&error));
        }
        if let Some(reason) = self.claim_secret(claim) {
            return Judgement::Refuse(reason);
        }
        if proposal.evidence.is_empty() {
            return Judgement::Refuse(ClaimReason::NoProvenance);
        }
        if let Some(source) = unavailable_source(proposal, context) {
            return Judgement::Refuse(ClaimReason::SourceUnavailable { source });
        }
        let statements = user_statements(proposal);
        let seeds: Vec<&SeedRef> = proposal
            .evidence
            .iter()
            .filter_map(|evidence| match evidence {
                Evidence::OperatorSeed { seed, .. } => Some(seed),
                _ => None,
            })
            .collect();
        let states_the_owners_word = proposal
            .evidence
            .iter()
            .any(|evidence| evidence.kind() == EvidenceKind::UserStatement);
        if !seeds.is_empty() && states_the_owners_word {
            return Judgement::Refuse(ClaimReason::PolicyDenied {
                rule: "seed_with_user_statement",
            });
        }
        if let Some(floor) = policy.doc.score_floor {
            let below = proposal.evidence.iter().any(
                |evidence| matches!(evidence, Evidence::ModelScore { score, .. } if *score < floor),
            );
            if below {
                return Judgement::Refuse(ClaimReason::BelowScoreFloor);
            }
        }
        if statements
            .iter()
            .any(|statement| !statement.asserted.is_user())
        {
            return Judgement::Refuse(ClaimReason::PolicyDenied {
                rule: "user_statement_below_user_level",
            });
        }

        let correcting = !statements.is_empty() && proposal.corrected().next().is_some();
        let Some(authority) = earned(proposal, policy, !statements.is_empty(), correcting) else {
            return Judgement::Refuse(ClaimReason::AuthorityInsufficient);
        };
        let owner = claim.scope.owner.as_str();
        let own_data = statements.iter().any(|statement| statement.by == owner);
        let sourcing = if own_data {
            Sourcing::User
        } else if !seeds.is_empty() {
            Sourcing::Seed
        } else {
            Sourcing::Supplier
        };

        let mut relations = Vec::with_capacity(proposal.relations.len());
        let mut conflicts = Vec::new();
        let mut outranked = false;
        for relation in &proposal.relations {
            let Some(target) = context.targets.get(&relation.to) else {
                return Judgement::Refuse(ClaimReason::PolicyDenied {
                    rule: "unknown_target",
                });
            };
            if target.scope != claim.scope {
                return Judgement::Refuse(ClaimReason::PolicyDenied {
                    rule: "cross_scope_relation",
                });
            }
            if relation.kind == RelationKind::Supersedes {
                if sourcing == Sourcing::User && target.sourcing == Sourcing::Supplier {
                    return Judgement::Refuse(ClaimReason::PolicyDenied {
                        rule: "user_supersedes_supplier",
                    });
                }
                outranked |= target.authority > authority;
            }
            if relation.kind == RelationKind::Contradicts
                && correcting
                && target.sourcing == Sourcing::Supplier
            {
                conflicts.push(relation.to);
            }
            match ClaimRelation::new(
                claim.id,
                relation.kind,
                RelationTarget::Claim(relation.to),
                claim.provenance.clone(),
            ) {
                Ok(built) => relations.push(built),
                Err(_) => {
                    return Judgement::Refuse(ClaimReason::PolicyDenied {
                        rule: "self_relation",
                    });
                }
            }
        }

        let reviewed = proposal
            .evidence
            .iter()
            .any(|evidence| matches!(evidence, Evidence::ReviewApproval { .. }));
        if outranked && !reviewed {
            return Judgement::Hold(ClaimReason::AuthorityInsufficient);
        }
        let seeded = !seeds.is_empty() && seeds.iter().all(|seed| policy.accepts(seed));
        let grounded = reviewed || own_data || seeded || policy.qualifies(claim, authority);
        if !grounded {
            return Judgement::Hold(ClaimReason::AuthorityInsufficient);
        }
        let corrects = if correcting {
            proposal.corrected().collect()
        } else {
            Vec::new()
        };
        Judgement::Admit(Box::new(AdmittedClaim {
            claim: claim.clone(),
            proposal: proposal.id,
            proposer: proposal.proposer.clone(),
            policy: policy.reference(),
            authority,
            sourcing,
            evidence: proposal.evidence.clone(),
            relations,
            corrects,
            conflicts,
        }))
    }

    /// The detectors of 013 over the claim's value, condition, slot and
    /// subject key, in that order.
    fn claim_secret(&self, claim: &Claim) -> Option<ClaimReason> {
        let value = match &claim.value {
            aicortex_types::ClaimValue::Text(text) => Some(text.as_str()),
            _ => None,
        };
        let fields = [
            (ClaimField::Value, value),
            (ClaimField::Condition, claim.epistemic.condition()),
            (
                ClaimField::Slot,
                claim.slot.as_ref().map(|slot| slot.as_str()),
            ),
            (ClaimField::Subject, Some(claim.subject.key.as_str())),
        ];
        fields.into_iter().find_map(|(field, text)| {
            let finding = crate::secrets::scan(text?, &self.rules().secrets)?;
            Some(ClaimReason::SecretDetected {
                field,
                detector: finding.detector,
                offset: finding.offset,
            })
        })
    }
}

enum Judgement {
    Admit(Box<AdmittedClaim>),
    Hold(ClaimReason),
    Refuse(ClaimReason),
}

/// A user statement the gate honours: made by a human proposer about
/// themselves. An agent's or someone else's is evidence of nothing (B-5).
struct Statement<'a> {
    by: &'a str,
    asserted: AuthorityLevel,
}

fn user_statements(proposal: &ClaimProposal) -> Vec<Statement<'_>> {
    proposal
        .evidence
        .iter()
        .filter_map(|evidence| match evidence {
            Evidence::UserStatement { by, authority }
                if proposal.proposer.is_human() && proposal.proposer.id.as_str() == by.as_str() =>
            {
                Some(Statement {
                    by: by.as_str(),
                    asserted: *authority,
                })
            }
            _ => None,
        })
        .collect()
}

/// The authority the evidence earns (B-5): the highest level any honoured
/// item reaches under the policy's ceilings. A user statement earns
/// `user_corrected` when the proposal corrects a claim.
fn earned(
    proposal: &ClaimProposal,
    policy: &AdmissionPolicy,
    honoured_statement: bool,
    correcting: bool,
) -> Option<AuthorityLevel> {
    proposal
        .evidence
        .iter()
        .filter_map(|evidence| {
            let kind = evidence.kind();
            let ceiling = policy.ceiling(kind)?;
            let level = match kind {
                EvidenceKind::UserStatement if !honoured_statement => return None,
                EvidenceKind::UserStatement if correcting => AuthorityLevel::UserCorrected,
                EvidenceKind::UserStatement => AuthorityLevel::UserAsserted,
                _ => ceiling,
            };
            Some(level.min(ceiling))
        })
        .max()
}

/// The first cited stored source that is not available.
fn unavailable_source(proposal: &ClaimProposal, context: &ClaimContext) -> Option<MemoryId> {
    let provenance = &proposal.claim.provenance;
    provenance
        .derived_from
        .iter()
        .chain(provenance.spans.iter().map(|span| &span.source))
        .chain(
            proposal
                .evidence
                .iter()
                .filter_map(Evidence::span)
                .map(|span| &span.source),
        )
        .find(|source| context.sources.get(source) != Some(&SourceState::Available))
        .copied()
}

/// The Decision for one admission batch (B-11, D-4): the admitted claim
/// ids, the policy, and the count. `batch` is every claim admitted in one
/// transaction.
#[must_use]
pub fn batch_entry(batch: &[AdmittedClaim], actor: &Sub) -> LedgerEntry {
    let ids: Vec<String> = batch
        .iter()
        .map(|admitted| admitted.claim.id.to_string())
        .collect();
    let policies: Vec<&PolicyRef> = {
        let mut policies: Vec<&PolicyRef> = batch.iter().map(AdmittedClaim::policy).collect();
        policies.sort();
        policies.dedup();
        policies
    };
    LedgerEntry {
        kind: KIND_CLAIM_BATCH,
        denied: false,
        actor: actor.clone(),
        reason: format!("admitted {} claims", batch.len()),
        payload: serde_json::json!({
            "claim_ids": ids,
            "policies": policies,
            "count": batch.len(),
        }),
    }
}

/// The Decision for one admitted user correction (B-11).
#[must_use]
pub fn correction_entry(admitted: &AdmittedClaim, actor: &Sub) -> LedgerEntry {
    let ids =
        |claims: &[ClaimId]| -> Vec<String> { claims.iter().map(ToString::to_string).collect() };
    LedgerEntry {
        kind: KIND_CLAIM_CORRECTION,
        denied: false,
        actor: actor.clone(),
        reason: format!(
            "a user correction of {} claims, {} kept in conflict",
            admitted.corrects.len(),
            admitted.conflicts.len()
        ),
        payload: serde_json::json!({
            "claim_id": admitted.claim.id.to_string(),
            "proposal_id": admitted.proposal.to_string(),
            "scope": admitted.claim.scope.to_string(),
            "corrects": ids(&admitted.corrects),
            "conflicts": ids(&admitted.conflicts),
            "authority": admitted.authority,
            "policy": admitted.policy,
        }),
    }
}

/// The Decision for a refused or held proposal (B-11). `digest` is the
/// keyed digest 013 B-9 requires for a refusal, over
/// [`crate::Gate::claim_digest_material`]; a hold carries none.
#[must_use]
pub fn proposal_entry(
    verdict: &ClaimVerdict,
    proposal: &ClaimProposal,
    policy: &PolicyRef,
    actor: &Sub,
    digest: Option<&DigestRef>,
) -> Option<LedgerEntry> {
    let (kind, reason) = match verdict {
        ClaimVerdict::Admit(_) => return None,
        ClaimVerdict::Hold(_, reason) => (KIND_CLAIM_HOLD, reason),
        ClaimVerdict::Refuse(reason) => (KIND_CLAIM_REFUSE, reason),
    };
    Some(LedgerEntry {
        kind,
        denied: kind == KIND_CLAIM_REFUSE,
        actor: actor.clone(),
        reason: reason.sentence(),
        payload: serde_json::json!({
            "reason_code": reason.code(),
            "reason": reason,
            "proposal_id": proposal.id.to_string(),
            "scope": proposal.claim.scope.to_string(),
            "predicate": proposal.claim.predicate.to_string(),
            "policy": policy,
            "digest": digest,
        }),
    })
}

/// The Decision for a new policy version (B-11, B-12). `digest` is the
/// document's digest, which the store computes.
#[must_use]
pub fn policy_entry(policy: &AdmissionPolicy, digest: &str, actor: &Sub) -> LedgerEntry {
    LedgerEntry {
        kind: KIND_POLICY_CHANGE,
        denied: false,
        actor: actor.clone(),
        reason: format!("admission policy {}@{}", policy.doc.id, policy.doc.version),
        payload: serde_json::json!({
            "policy_id": policy.doc.id,
            "version": policy.doc.version,
            "digest": digest,
            "qualified": policy.doc.qualified.len(),
            "seeds": policy.doc.seeds.len(),
        }),
    }
}
