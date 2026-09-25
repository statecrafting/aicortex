//! What an actor offers for a claim, and how much it is worth (spec 051).
//!
//! Three layers are kept apart (B-1 to B-3): the observation is a stored
//! memory, a [`ClaimProposal`] is a typed claim some actor suggests with its
//! [`Evidence`], and an admitted claim is what `aicortex-gate` makes of a
//! proposal it accepts. This module holds the middle layer and the
//! vocabulary the gate judges it in; nothing here admits anything.
//!
//! Two properties are carried by the types:
//!
//! - **A score is a number, never a level.** [`Score`] is fixed-point and
//!   lives only inside [`Evidence::ModelScore`]; [`AuthorityLevel`] has no
//!   conversion from it (B-6).
//! - **Authority is assigned, never supplied.** A proposal carries no
//!   authority field. The one level an actor writes, inside a
//!   [`Evidence::UserStatement`], is recorded as evidence and recomputed by
//!   the gate (B-5).

use core::fmt;
use core::str::FromStr;

use rahi_types::{Sub, UnixSeconds};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::actor::Actor;
use crate::claim::{Claim, ClaimId, RelationKind};
use crate::claim_time::{checked_string, closed_vocabulary};
use crate::error::{Result, TypeError, validate_key};
use crate::provenance::ExtractorVersion;
use crate::span::SourceSpan;
use crate::trust::DecisionRef;

/// A proposal's identifier (B-2): a UUID v7 minted by the writer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProposalId(Uuid);

impl ProposalId {
    /// Mint an identifier for a proposal being made now.
    #[must_use]
    pub fn now_v7() -> Self {
        Self(Uuid::now_v7())
    }

    /// Carry an identifier minted elsewhere.
    #[must_use]
    pub const fn from_uuid(uuid: Uuid) -> Self {
        Self(uuid)
    }

    /// Parse a hyphenated UUID.
    ///
    /// # Errors
    ///
    /// [`TypeError::Invalid`] naming `proposal_id` when the text is not a
    /// UUID.
    pub fn parse(text: &str) -> Result<Self> {
        Uuid::parse_str(text)
            .map(Self)
            .map_err(|error| TypeError::Invalid {
                field: "proposal_id",
                reason: format!("{text:?} is not a uuid: {error}"),
            })
    }
}

impl FromStr for ProposalId {
    type Err = TypeError;

    fn from_str(text: &str) -> Result<Self> {
        Self::parse(text)
    }
}

impl fmt::Display for ProposalId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.as_hyphenated())
    }
}

closed_vocabulary! {
    /// How far an admitted claim may be relied on (B-5). Closed and totally
    /// ordered in declaration order: `inferred` is the least.
    AuthorityLevel, "authority", AUTHORITY_LEVELS {
        /// Only a model's inference supports it.
        Inferred => "inferred",
        /// A rule, a model extraction with a span, or an operator seed.
        Extracted => "extracted",
        /// A deterministic check re-derived it from the source bytes.
        Verified => "verified",
        /// The scope's human said so.
        UserAsserted => "user_asserted",
        /// The scope's human said so, correcting an existing claim.
        UserCorrected => "user_corrected",
    }
}

impl AuthorityLevel {
    /// Whether this is a level only a human's own statement reaches.
    #[must_use]
    pub const fn is_user(self) -> bool {
        matches!(self, Self::UserAsserted | Self::UserCorrected)
    }
}

closed_vocabulary! {
    /// Where an admitted claim came from, for the correction rule (B-8).
    Sourcing, "sourcing", SOURCINGS {
        /// Admitted on a statement by the owner of its scope.
        User => "user",
        /// Admitted as a deployment's operator seed (B-14).
        Seed => "seed",
        /// Anything else: a supplier's message, or what was extracted from
        /// one.
        Supplier => "supplier",
    }
}

closed_vocabulary! {
    /// The kinds of [`Evidence`], as a policy names them (B-12).
    EvidenceKind, "evidence", EVIDENCE_KINDS {
        /// [`Evidence::SourceSpan`].
        SourceSpan => "source_span",
        /// [`Evidence::SpanVerified`].
        SpanVerified => "span_verified",
        /// [`Evidence::ModelScore`].
        ModelScore => "model_score",
        /// [`Evidence::RuleMatch`].
        RuleMatch => "rule_match",
        /// [`Evidence::UserStatement`].
        UserStatement => "user_statement",
        /// [`Evidence::Corroboration`].
        Corroboration => "corroboration",
        /// [`Evidence::ReviewApproval`].
        ReviewApproval => "review_approval",
        /// [`Evidence::OperatorSeed`].
        OperatorSeed => "operator_seed",
    }
}

/// A model's confidence as fixed-point basis points in `[0, 10000]`, that is
/// `[0, 1]` in steps of `0.0001` (B-4). There is no float here, and no
/// arithmetic: a score is compared, never combined.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct Score(u16);

impl Score {
    /// The basis points of a score of one.
    pub const SCALE: u16 = 10_000;

    /// A score of `basis_points / 10000`.
    ///
    /// # Errors
    ///
    /// [`TypeError::Invalid`] naming `score` above [`Self::SCALE`].
    pub fn new(basis_points: u16) -> Result<Self> {
        if basis_points > Self::SCALE {
            return Err(TypeError::Invalid {
                field: "score",
                reason: format!("{basis_points} is over {}", Self::SCALE),
            });
        }
        Ok(Self(basis_points))
    }

    /// The basis points.
    #[must_use]
    pub const fn basis_points(self) -> u16 {
        self.0
    }
}

impl<'de> Deserialize<'de> for Score {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> core::result::Result<Self, D::Error> {
        Self::new(u16::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

checked_string! {
    /// The name of a deployment's seed set (B-14).
    SeedSet, "seed_set", validate_key
}

/// One version of one seed set (B-14).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SeedRef {
    /// The set.
    pub set: SeedSet,
    /// Its version.
    pub version: u32,
}

/// What supports a proposal (B-4). Closed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Evidence {
    /// The bytes that support the value.
    SourceSpan {
        /// The span.
        span: SourceSpan,
    },
    /// A deterministic check re-derived the value from the span's bytes.
    SpanVerified {
        /// The span it re-derived the value from.
        span: SourceSpan,
        /// The check and its version.
        method: ExtractorVersion,
    },
    /// A model's confidence, with the model and version that produced it.
    ModelScore {
        /// The model.
        model: ExtractorVersion,
        /// The confidence.
        score: Score,
        /// What the score is a confidence in.
        label: String,
    },
    /// A deterministic extraction rule fired.
    RuleMatch {
        /// The rule and its version.
        rule: ExtractorVersion,
    },
    /// A human said so, directly or as a correction (B-8).
    UserStatement {
        /// Who.
        by: Sub,
        /// The level the statement asserts. Recorded, and recomputed by the
        /// gate, which never takes it on trust (B-5).
        authority: AuthorityLevel,
    },
    /// Other admitted claims that agree.
    Corroboration {
        /// The agreeing claims.
        claims: Vec<ClaimId>,
    },
    /// A human reviewer approved admission of this proposal (B-13).
    ReviewApproval {
        /// The reviewer.
        by: Sub,
        /// The ledger Decision that records the approval.
        decision: DecisionRef,
    },
    /// The value is a deployment's seed data (B-14).
    OperatorSeed {
        /// The operator who loaded it.
        by: Sub,
        /// The seed set and version.
        seed: SeedRef,
    },
}

impl Evidence {
    /// Which kind of evidence this is.
    #[must_use]
    pub const fn kind(&self) -> EvidenceKind {
        match self {
            Self::SourceSpan { .. } => EvidenceKind::SourceSpan,
            Self::SpanVerified { .. } => EvidenceKind::SpanVerified,
            Self::ModelScore { .. } => EvidenceKind::ModelScore,
            Self::RuleMatch { .. } => EvidenceKind::RuleMatch,
            Self::UserStatement { .. } => EvidenceKind::UserStatement,
            Self::Corroboration { .. } => EvidenceKind::Corroboration,
            Self::ReviewApproval { .. } => EvidenceKind::ReviewApproval,
            Self::OperatorSeed { .. } => EvidenceKind::OperatorSeed,
        }
    }

    /// The span this evidence points into, when it points into one.
    #[must_use]
    pub const fn span(&self) -> Option<&SourceSpan> {
        match self {
            Self::SourceSpan { span } | Self::SpanVerified { span, .. } => Some(span),
            _ => None,
        }
    }
}

/// A relation a proposal asks for, from its claim to an existing claim
/// (B-8, B-10). The gate turns an admitted one into a `ClaimRelation`
/// carrying the claim's provenance.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProposedRelation {
    /// How the proposed claim relates to the target.
    pub kind: RelationKind,
    /// The existing claim.
    pub to: ClaimId,
}

/// A claim some actor suggests, with its evidence (B-2).
///
/// Appended to `claim_proposal`, never visible to a projection, and never
/// stored as a claim except through the gate.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimProposal {
    /// The writer-minted identifier.
    pub id: ProposalId,
    /// What is proposed.
    pub claim: Claim,
    /// Who proposes it.
    pub proposer: Actor,
    /// What supports it, in the order offered. An evidence item's id is its
    /// position here, which a stored proposal never changes.
    pub evidence: Vec<Evidence>,
    /// The relations it asks for, to existing claims.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relations: Vec<ProposedRelation>,
    /// When it was proposed.
    pub proposed_at: UnixSeconds,
}

impl ClaimProposal {
    /// The claims this proposal corrects: the targets of its `supersedes`
    /// and `contradicts` relations.
    pub fn corrected(&self) -> impl Iterator<Item = ClaimId> + '_ {
        self.relations
            .iter()
            .filter(|relation| {
                matches!(
                    relation.kind,
                    RelationKind::Supersedes | RelationKind::Contradicts
                )
            })
            .map(|relation| relation.to)
    }
}
