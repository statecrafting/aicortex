//! The gate against the evaluators spec 057 replaces (057 FR-002, D-6).
//!
//! `oracle` below is `Gate::evaluate`'s fault chain and `Gate::evaluate_claim`'s
//! `judge` as they stood before spec 057 (aicortex `6f81d5a`), reproduced
//! over the crate's public API. Each generated input is judged by the oracle
//! and by `Gate`, and the verdicts must be equal: the label, the reason with every payload
//! field, and the admitted memory or claim. Equal reasons serialize to equal
//! bytes, so the ledger entries built from them are equal too.
//!
//! The inputs are 013's and 051's committed corpora, varied one axis at a
//! time and then sampled across every axis at once with a fixed seed, so a
//! run is reproducible. Coverage is asserted, not hoped for: every verdict,
//! every reason code and every 051 rule name occurs, as do inputs that break
//! several rules at once.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use aicortex_claims::RegistrySnapshot;
use aicortex_gate::{
    AdmissionPolicy, Candidate, ClaimContext, ClaimReason, ClaimVerdict, Gate, Limits, Origin,
    PolicyDocument, RuleSet, SourceState, TargetFacts, Verdict,
};
use aicortex_types::{
    Actor, ActorId, AgentOrigin, Claim, ClaimId, ClaimParts, ClaimProposal, ClaimValue,
    ContentDigest, EpistemicStatus, Evidence, Hex, MemoryId, PartLocator, PredicateRef,
    PredicateSet, ProposalId, ProposedRelation, Provenance, Scope, SlotKey, SourceRef, SourceSpan,
    SourceSystem, SpanRange, SpanUnit, SubjectKey, SubjectKind, SubjectRef,
};
use rahi_types::{Sub, UnixSeconds};
use serde::Deserialize;

/// The pre-change evaluators, reproduced from `6f81d5a` over the public API.
mod oracle {
    use aicortex_claims::{ClaimError, RegistrySnapshot};
    use aicortex_gate::{
        AdmissionPolicy, Candidate, ClaimContext, ClaimField, ClaimReason, MediaFault, Reason,
        RuleSet, SourceState, normalize, secrets,
    };
    use aicortex_types::{
        AuthorityLevel, Claim, ClaimId, ClaimProposal, ClaimRelation, Evidence, EvidenceKind,
        Memory, MemoryBody, MemoryId, RelationKind, RelationTarget, SeedRef, Sourcing, Status,
    };

    /// What the old `Gate::evaluate` answered, with the memory it admitted.
    #[derive(Debug, PartialEq)]
    pub enum Capture {
        Admit(Memory),
        Quarantine(Memory, Reason),
        Refuse(Reason),
    }

    pub fn evaluate(rules: &RuleSet, candidate: &Candidate) -> Capture {
        match fault(rules, candidate) {
            Some(reason) => Capture::Refuse(reason),
            None if candidate.origin.is_established() => Capture::Admit(normalized(candidate)),
            None if candidate.origin.is_assertable() => {
                let mut memory = normalized(candidate);
                memory.status = Status::Quarantined;
                Capture::Quarantine(
                    memory,
                    Reason::OriginUnestablished {
                        origin: candidate.origin,
                    },
                )
            }
            None => Capture::Refuse(Reason::OriginUnestablished {
                origin: candidate.origin,
            }),
        }
    }

    fn fault(rules: &RuleSet, candidate: &Candidate) -> Option<Reason> {
        let body = normalize::body(&candidate.parts.body);
        if body.text.trim().is_empty() {
            return Some(Reason::EmptyAfterNormalization);
        }
        let bytes = body.text.len();
        let ceiling = rules.limits.max_body_bytes;
        if bytes > ceiling {
            return Some(Reason::TooLarge { bytes, ceiling });
        }
        if let Some(fault) = media_fault(rules, &body) {
            return Some(Reason::UnsupportedMedia(fault));
        }
        if rules
            .denied_sources
            .contains(&candidate.parts.provenance.source.system)
        {
            return Some(Reason::PolicyDenied {
                policy: format!("denied-source:{}", candidate.parts.provenance.source.system),
            });
        }
        secret_fault(rules, &body)
    }

    fn media_fault(rules: &RuleSet, body: &MemoryBody) -> Option<MediaFault> {
        let count = body.media.len();
        let ceiling = rules.limits.max_media_refs;
        if count > ceiling {
            return Some(MediaFault::TooMany { count, ceiling });
        }
        body.media
            .iter()
            .position(|media| !rules.limits.admits_media(media))
            .map(|index| MediaFault::UnsupportedType { index })
    }

    fn secret_fault(rules: &RuleSet, body: &MemoryBody) -> Option<Reason> {
        let in_text = secrets::scan(&body.text, &rules.secrets);
        let in_title = body
            .title
            .as_deref()
            .and_then(|title| secrets::scan(title, &rules.secrets));
        in_text.or(in_title).map(|finding| Reason::SecretDetected {
            detector: finding.detector,
            offset: finding.offset,
        })
    }

    fn normalized(candidate: &Candidate) -> Memory {
        let mut parts = candidate.parts.clone();
        parts.body = normalize::body(&parts.body);
        Memory::new(parts)
    }

    /// What the old `judge` answered; an admission as its fields.
    #[derive(Debug, PartialEq)]
    pub enum Judgement {
        Admit(Admission),
        Hold(ClaimReason),
        Refuse(ClaimReason),
    }

    #[derive(Debug, PartialEq)]
    pub struct Admission {
        pub authority: AuthorityLevel,
        pub sourcing: Sourcing,
        pub relations: Vec<ClaimRelation>,
        pub corrects: Vec<ClaimId>,
        pub conflicts: Vec<ClaimId>,
    }

    fn invalid(error: &ClaimError) -> ClaimReason {
        match error {
            ClaimError::UnknownPredicate(_)
            | ClaimError::WrongVersion(_)
            | ClaimError::Withdrawn(_) => ClaimReason::UnregisteredPredicate,
            other => ClaimReason::InvalidValue { rule: other.code() },
        }
    }

    fn qualifies(policy: &AdmissionPolicy, claim: &Claim, authority: AuthorityLevel) -> bool {
        policy.document().qualified.iter().any(|qualification| {
            qualification.namespace == claim.predicate.namespace
                && qualification.predicate == claim.predicate.name
                && authority >= qualification.min_authority
        })
    }

    fn accepts(policy: &AdmissionPolicy, seed: &SeedRef) -> bool {
        policy
            .document()
            .seeds
            .iter()
            .any(|accepted| &accepted.seed == seed)
    }

    pub fn judge(
        secrets: &aicortex_gate::SecretRules,
        proposal: &ClaimProposal,
        registry: &RegistrySnapshot,
        policy: &AdmissionPolicy,
        context: &ClaimContext,
    ) -> Judgement {
        let claim = &proposal.claim;
        if let Err(error) = aicortex_claims::validate(claim, registry) {
            return Judgement::Refuse(invalid(&error));
        }
        if let Some(reason) = claim_secret(secrets, claim) {
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
        if let Some(floor) = policy.document().score_floor {
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
        let seeded = !seeds.is_empty() && seeds.iter().all(|seed| accepts(policy, seed));
        let grounded = reviewed || own_data || seeded || qualifies(policy, claim, authority);
        if !grounded {
            return Judgement::Hold(ClaimReason::AuthorityInsufficient);
        }
        let corrects = if correcting {
            proposal.corrected().collect()
        } else {
            Vec::new()
        };
        Judgement::Admit(Admission {
            authority,
            sourcing,
            relations,
            corrects,
            conflicts,
        })
    }

    fn claim_secret(rules: &aicortex_gate::SecretRules, claim: &Claim) -> Option<ClaimReason> {
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
            let finding = secrets::scan(text?, rules)?;
            Some(ClaimReason::SecretDetected {
                field,
                detector: finding.detector,
                offset: finding.offset,
            })
        })
    }

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
                    if proposal.proposer.is_human()
                        && proposal.proposer.id.as_str() == by.as_str() =>
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

    pub fn unavailable_source(
        proposal: &ClaimProposal,
        context: &ClaimContext,
    ) -> Option<MemoryId> {
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
}

/// A fixed-seed generator (SplitMix64), so every run samples the same inputs.
struct Seeded(u64);

impl Seeded {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, bound: usize) -> usize {
        usize::try_from(self.next() % u64::try_from(bound).unwrap()).unwrap()
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
}

// --- 013: capture -----------------------------------------------------------

fn rule_sets() -> Vec<RuleSet> {
    let corpus = SourceSystem::new("corpus").unwrap();
    let narrow = |bytes: usize, refs: usize, levels: &[&str]| {
        let limits = Limits {
            max_body_bytes: bytes,
            max_media_refs: refs,
            media_top_levels: levels.iter().map(|level| (*level).to_owned()).collect(),
        };
        RuleSet::standard().narrowed_to(limits).unwrap()
    };
    vec![
        RuleSet::standard(),
        RuleSet::standard().denying(corpus.clone()),
        narrow(64, 16, &["image", "text"]),
        narrow(64 * 1024, 0, &["image"]),
        narrow(32, 1, &["text"]).denying(corpus),
    ]
}

const ORIGINS: [Origin; 4] = [
    Origin::Authenticated,
    Origin::RegisteredAdapter,
    Origin::Asserted,
    Origin::Unestablished,
];

fn label(verdict: &Verdict) -> &'static str {
    match verdict {
        Verdict::Admit(_) => "admit",
        Verdict::Quarantine(..) => "quarantine",
        Verdict::Refuse(_) => "refuse",
    }
}

/// Judge `candidate` both ways and return the verdict, or fail naming it.
fn same_capture(rules: &RuleSet, candidate: &Candidate, case: &str) -> Verdict {
    let verdict = Gate::new(rules.clone()).evaluate(candidate);
    let expected = oracle::evaluate(rules, candidate);
    let matches = match (&verdict, &expected) {
        (Verdict::Admit(admitted), oracle::Capture::Admit(memory)) => admitted.memory() == memory,
        (Verdict::Quarantine(admitted, reason), oracle::Capture::Quarantine(memory, expected)) => {
            admitted.memory() == memory && reason == expected
        }
        (Verdict::Refuse(reason), oracle::Capture::Refuse(expected)) => reason == expected,
        _ => false,
    };
    assert!(
        matches,
        "{case}: the walk answered {verdict:?}, the old evaluator {expected:?}"
    );
    if let (Some(reason), oracle::Capture::Refuse(expected)) = (verdict.reason(), &expected) {
        assert_eq!(
            serde_json::to_vec(reason).unwrap(),
            serde_json::to_vec(expected).unwrap(),
            "{case}: the reason bytes differ"
        );
    }
    verdict
}

/// How many of 013's faults `candidate` carries, each checked on its own.
fn capture_faults(rules: &RuleSet, candidate: &Candidate) -> usize {
    let body = aicortex_gate::normalize::body(&candidate.parts.body);
    let secret = aicortex_gate::secrets::scan(&body.text, &rules.secrets).is_some()
        || body
            .title
            .as_deref()
            .is_some_and(|title| aicortex_gate::secrets::scan(title, &rules.secrets).is_some());
    [
        body.text.trim().is_empty(),
        body.text.len() > rules.limits.max_body_bytes,
        body.media.len() > rules.limits.max_media_refs
            || body
                .media
                .iter()
                .any(|media| !rules.limits.admits_media(media)),
        rules
            .denied_sources
            .contains(&candidate.parts.provenance.source.system),
        secret,
        !candidate.origin.is_established() && !candidate.origin.is_assertable(),
    ]
    .into_iter()
    .filter(|fault| *fault)
    .count()
}

#[test]
fn fr002_capture_walk_matches_the_old_evaluator_over_the_corpus_and_its_variations() {
    let fixtures: Vec<common::Fixture> = common::corpus()
        .into_iter()
        .filter(|fixture| fixture.kind == "verdict")
        .collect();
    assert!(fixtures.len() >= 30, "only {} fixtures", fixtures.len());
    let rule_sets = rule_sets();
    let mut labels = BTreeSet::new();
    let mut codes = BTreeSet::new();
    let mut multi = 0usize;
    let mut cases = 0usize;
    for fixture in &fixtures {
        let base = fixture.candidate();
        for (index, rules) in rule_sets.iter().enumerate() {
            for origin in ORIGINS {
                let candidate = Candidate::new(base.parts.clone(), origin);
                let case = format!("{} rules#{index} {origin:?}", fixture.name);
                let verdict = same_capture(rules, &candidate, &case);
                labels.insert(label(&verdict));
                if let Some(reason) = verdict.reason() {
                    codes.insert(reason.code());
                }
                if capture_faults(rules, &candidate) >= 2 {
                    multi += 1;
                }
                cases += 1;
            }
        }
    }
    eprintln!("013: {cases} cases, {multi} with two or more faults");
    assert_eq!(
        labels,
        BTreeSet::from(["admit", "quarantine", "refuse"]),
        "a verdict was never reached"
    );
    for code in [
        "empty_after_normalization",
        "too_large",
        "unsupported_media",
        "policy_denied",
        "secret_detected",
        "origin_unestablished",
    ] {
        assert!(codes.contains(code), "{code} was never produced");
    }
    assert!(multi >= 100, "only {multi} multi-fault cases");
}

// --- 051: claims ------------------------------------------------------------

const OWNER: &str = "sub-owner";
const STRANGER: &str = "sub-stranger";
const OPERATOR: &str = "sub-operator";
const REVIEWER: &str = "sub-reviewer";
const SOURCE: &str = "019c4f00-0000-7000-8000-0000000000a1";
const TARGET: &str = "019c4f00-0000-7000-8000-0000000000d4";
const CLAIM: &str = "019c4f00-0000-7000-8000-0000000000c3";
const PROPOSAL: &str = "019c4f00-0000-7000-8000-0000000000e5";

#[derive(Clone, Debug, Deserialize)]
struct CaseSubject {
    kind: String,
    key: String,
}

#[derive(Clone, Debug, Deserialize)]
struct CaseClaim {
    predicate: String,
    subject: CaseSubject,
    value: ClaimValue,
    epistemic: EpistemicStatus,
    #[serde(default)]
    slot: Option<String>,
    #[serde(default)]
    derived: bool,
}

#[derive(Clone, Debug, Deserialize)]
struct ClaimFixture {
    name: String,
    proposer: String,
    claim: CaseClaim,
    evidence: Vec<Evidence>,
    #[serde(default)]
    relations: Vec<ProposedRelation>,
    #[serde(default)]
    policy: Option<PolicyDocument>,
    #[serde(default)]
    sources: Option<BTreeMap<MemoryId, SourceState>>,
    #[serde(default)]
    targets: BTreeMap<ClaimId, ClaimTarget>,
}

#[derive(Clone, Debug, Deserialize)]
struct ClaimTarget {
    authority: aicortex_types::AuthorityLevel,
    sourcing: aicortex_types::Sourcing,
}

fn span() -> SourceSpan {
    SourceSpan {
        source: MemoryId::parse(SOURCE).unwrap(),
        part: PartLocator::Body,
        range: SpanRange::new(SpanUnit::Chars, 120, 126).unwrap(),
        digest: ContentDigest {
            algorithm: "HMAC-SHA-256".to_owned(),
            key_id: Hex::new("0f".repeat(16)).unwrap(),
            digest: Hex::new("ab".repeat(32)).unwrap(),
        },
    }
}

/// The placeholders of `tests/claim_admission.rs`, in values and keys.
fn substitute(value: &mut serde_json::Value) {
    let replace = |text: &str| -> Option<serde_json::Value> {
        Some(match text {
            "$SPAN" => serde_json::to_value(span()).unwrap(),
            "$OWNER" => OWNER.into(),
            "$STRANGER" => STRANGER.into(),
            "$OPERATOR" => OPERATOR.into(),
            "$REVIEWER" => REVIEWER.into(),
            "$SOURCE" => SOURCE.into(),
            "$TARGET" => TARGET.into(),
            _ => return None,
        })
    };
    match value {
        serde_json::Value::String(text) => {
            if let Some(replaced) = replace(text) {
                *value = replaced;
            }
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(substitute),
        serde_json::Value::Object(map) => {
            let entries: Vec<(String, serde_json::Value)> = std::mem::take(map)
                .into_iter()
                .map(|(key, mut item)| {
                    substitute(&mut item);
                    let key = match replace(&key) {
                        Some(serde_json::Value::String(replaced)) => replaced,
                        _ => key,
                    };
                    (key, item)
                })
                .collect();
            map.extend(entries);
        }
        _ => {}
    }
}

fn claim_corpus() -> Vec<ClaimFixture> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testdata/claims");
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            let mut value: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            substitute(&mut value);
            serde_json::from_value(value)
                .unwrap_or_else(|error| panic!("{}: {error}", path.display()))
        })
        .collect()
}

fn registry() -> RegistrySnapshot {
    let base =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../aicortex-claims/testdata/registries");
    let sets = ["travel-v1.json", "travel-v2.json"].map(|name| {
        serde_json::from_str::<PredicateSet>(&std::fs::read_to_string(base.join(name)).unwrap())
            .unwrap()
    });
    RegistrySnapshot::from_sets(sets).unwrap()
}

fn scope() -> Scope {
    Scope::personal(Sub::new(OWNER))
}

const PROPOSERS: [&str; 4] = ["owner", "stranger", "agent", "operator"];

fn proposer(role: &str) -> Actor {
    match role {
        "owner" => Actor::human(ActorId::new(OWNER).unwrap()),
        "stranger" => Actor::human(ActorId::new(STRANGER).unwrap()),
        "agent" => Actor::agent(ActorId::new("extractor").unwrap(), AgentOrigin::default()),
        "operator" => Actor::system(ActorId::new("seeder").unwrap()),
        other => panic!("unknown proposer role {other}"),
    }
}

fn claim_of(case: &CaseClaim) -> Claim {
    let predicate = PredicateRef::parse(&case.predicate).unwrap();
    let at = UnixSeconds::new(1_789_041_600);
    let mut provenance = Provenance::captured(
        SourceRef::new(SourceSystem::new("travel-ingest").unwrap()),
        at,
        at,
    );
    if case.derived {
        provenance.derived_from = vec![MemoryId::parse(SOURCE).unwrap()];
        provenance = provenance.with_spans(vec![span()]);
    }
    Claim::new(ClaimParts {
        id: ClaimId::parse(CLAIM).unwrap(),
        scope: scope(),
        subject: SubjectRef {
            namespace: predicate.namespace.clone(),
            kind: SubjectKind::new(case.subject.kind.clone()).unwrap(),
            key: SubjectKey::new(case.subject.key.clone()).unwrap(),
        },
        predicate,
        value: case.value.clone(),
        slot: case.slot.clone().map(|slot| SlotKey::new(slot).unwrap()),
        epistemic: case.epistemic.clone(),
        provenance,
    })
}

/// One generated claim input: every axis the gate reads.
#[derive(Clone)]
struct ClaimCase {
    claim: CaseClaim,
    proposer: &'static str,
    evidence: Vec<Evidence>,
    relations: Vec<ProposedRelation>,
    policy: AdmissionPolicy,
    sources: BTreeMap<MemoryId, SourceState>,
    targets: BTreeMap<ClaimId, TargetFacts>,
}

impl ClaimCase {
    fn of(fixture: &ClaimFixture) -> Self {
        let sources = fixture.sources.clone().unwrap_or_else(|| {
            if fixture.claim.derived {
                BTreeMap::from([(MemoryId::parse(SOURCE).unwrap(), SourceState::Available)])
            } else {
                BTreeMap::new()
            }
        });
        let targets = fixture
            .targets
            .iter()
            .map(|(id, target)| (*id, facts(target.authority, target.sourcing, scope())))
            .collect();
        Self {
            claim: fixture.claim.clone(),
            proposer: PROPOSERS
                .into_iter()
                .find(|role| *role == fixture.proposer)
                .unwrap(),
            evidence: fixture.evidence.clone(),
            relations: fixture.relations.clone(),
            policy: fixture
                .policy
                .clone()
                .map_or_else(AdmissionPolicy::initial, |doc| {
                    AdmissionPolicy::new(doc).unwrap()
                }),
            sources,
            targets,
        }
    }

    fn proposal(&self) -> ClaimProposal {
        ClaimProposal {
            id: ProposalId::parse(PROPOSAL).unwrap(),
            claim: claim_of(&self.claim),
            proposer: proposer(self.proposer),
            evidence: self.evidence.clone(),
            relations: self.relations.clone(),
            proposed_at: UnixSeconds::new(1_789_041_660),
        }
    }

    fn context(&self) -> ClaimContext {
        ClaimContext {
            sources: self.sources.clone(),
            targets: self.targets.clone(),
        }
    }
}

fn facts(
    authority: aicortex_types::AuthorityLevel,
    sourcing: aicortex_types::Sourcing,
    scope: Scope,
) -> TargetFacts {
    TargetFacts {
        scope,
        authority,
        sourcing,
    }
}

/// The value pools the variations draw on, gathered from the corpus and
/// widened with the inputs it has no fixture for.
struct Pools {
    claims: Vec<CaseClaim>,
    evidence: Vec<Evidence>,
    relations: Vec<Vec<ProposedRelation>>,
    policies: Vec<AdmissionPolicy>,
    sources: Vec<BTreeMap<MemoryId, SourceState>>,
    targets: Vec<BTreeMap<ClaimId, TargetFacts>>,
}

fn pools(corpus: &[ClaimFixture]) -> Pools {
    let mut evidence: Vec<Evidence> = Vec::new();
    for item in corpus.iter().flat_map(|fixture| fixture.evidence.iter()) {
        if !evidence.contains(item) {
            evidence.push(item.clone());
        }
    }
    let mut policies = vec![AdmissionPolicy::initial()];
    for doc in corpus.iter().filter_map(|fixture| fixture.policy.clone()) {
        let policy = AdmissionPolicy::new(doc).unwrap();
        if !policies.contains(&policy) {
            policies.push(policy);
        }
    }
    // Every rule of the corpus's policies at once, under lowered ceilings.
    let mut combined = policies.iter().fold(
        AdmissionPolicy::initial().document().clone(),
        |mut doc, policy| {
            let other = policy.document();
            doc.score_floor = doc.score_floor.or(other.score_floor);
            doc.qualified.extend(other.qualified.iter().cloned());
            doc.seeds.extend(other.seeds.iter().cloned());
            doc
        },
    );
    combined.id = "travel.admission".to_owned();
    combined.version = 9;
    combined.ceilings = serde_json::from_value(serde_json::json!([
        { "evidence": "span_verified", "level": "extracted" },
        { "evidence": "source_span", "level": "inferred" },
    ]))
    .unwrap();
    policies.push(AdmissionPolicy::new(combined).unwrap());

    let source = MemoryId::parse(SOURCE).unwrap();
    let sources = vec![
        BTreeMap::new(),
        BTreeMap::from([(source, SourceState::Available)]),
        BTreeMap::from([(source, SourceState::Quarantined)]),
        BTreeMap::from([(source, SourceState::Erased)]),
    ];

    let target = ClaimId::parse(TARGET).unwrap();
    let own = ClaimId::parse(CLAIM).unwrap();
    let mut targets = vec![BTreeMap::new()];
    for fixture in corpus {
        for (id, facts_of) in &fixture.targets {
            let map =
                BTreeMap::from([(*id, facts(facts_of.authority, facts_of.sourcing, scope()))]);
            if !targets.contains(&map) {
                targets.push(map);
            }
        }
    }
    let elsewhere = Scope::personal(Sub::new(STRANGER));
    targets.push(BTreeMap::from([(
        target,
        facts(
            aicortex_types::AuthorityLevel::Extracted,
            aicortex_types::Sourcing::Supplier,
            elsewhere,
        ),
    )]));
    targets.push(BTreeMap::from([
        (
            target,
            facts(
                aicortex_types::AuthorityLevel::Extracted,
                aicortex_types::Sourcing::Supplier,
                scope(),
            ),
        ),
        (
            own,
            facts(
                aicortex_types::AuthorityLevel::Extracted,
                aicortex_types::Sourcing::Supplier,
                scope(),
            ),
        ),
    ]));

    let relation = |kind: &str, to: &str| -> ProposedRelation {
        serde_json::from_value(serde_json::json!({ "kind": kind, "to": to })).unwrap()
    };
    let relations = vec![
        Vec::new(),
        vec![relation("supersedes", TARGET)],
        vec![relation("contradicts", TARGET)],
        vec![
            relation("contradicts", TARGET),
            relation("supersedes", TARGET),
        ],
        vec![relation("supersedes", CLAIM)],
    ];

    let mut claims: Vec<CaseClaim> = corpus.iter().map(|fixture| fixture.claim.clone()).collect();
    let mut derived = claims[0].clone();
    derived.derived = !derived.derived;
    claims.push(derived);

    Pools {
        claims,
        evidence,
        relations,
        policies,
        sources,
        targets,
    }
}

/// The admitted claim's fields, for comparison with the oracle's.
fn admission_of(verdict: &ClaimVerdict) -> Option<oracle::Admission> {
    verdict.admitted().map(|admitted| oracle::Admission {
        authority: admitted.authority(),
        sourcing: admitted.sourcing(),
        relations: admitted.relations().to_vec(),
        corrects: admitted.corrects().to_vec(),
        conflicts: admitted.conflicts().to_vec(),
    })
}

/// Judge `case` both ways and return the verdict, or fail naming it.
fn same_claim(
    gate: &Gate,
    registry: &RegistrySnapshot,
    case: &ClaimCase,
    name: &str,
) -> ClaimVerdict {
    let proposal = case.proposal();
    let context = case.context();
    let verdict = gate.evaluate_claim(&proposal, registry, &case.policy, &context);
    let expected = oracle::judge(
        &gate.rules().secrets,
        &proposal,
        registry,
        &case.policy,
        &context,
    );
    let matches = match (&verdict, &expected) {
        (ClaimVerdict::Refuse(reason), oracle::Judgement::Refuse(expected)) => reason == expected,
        (ClaimVerdict::Hold(held, reason), oracle::Judgement::Hold(expected)) => {
            **held == proposal && reason == expected
        }
        (ClaimVerdict::Admit(admitted), oracle::Judgement::Admit(expected)) => {
            admission_of(&verdict).as_ref() == Some(expected)
                && admitted.claim() == &proposal.claim
                && admitted.proposal() == proposal.id
                && admitted.proposer() == &proposal.proposer
                && admitted.policy() == &case.policy.reference()
                && admitted.evidence() == proposal.evidence.as_slice()
        }
        _ => false,
    };
    assert!(
        matches,
        "{name}: the walk answered {verdict:?}, the old evaluator {expected:?}"
    );
    if let Some(reason) = verdict.reason() {
        let expected = match &expected {
            oracle::Judgement::Refuse(reason) | oracle::Judgement::Hold(reason) => reason,
            oracle::Judgement::Admit(_) => unreachable!(),
        };
        assert_eq!(
            serde_json::to_vec(reason).unwrap(),
            serde_json::to_vec(expected).unwrap(),
            "{name}: the reason bytes differ"
        );
    }
    verdict
}

/// How many of 051's leading faults `case` carries, each checked on its own.
fn claim_faults(gate: &Gate, registry: &RegistrySnapshot, case: &ClaimCase) -> usize {
    let proposal = case.proposal();
    let claim = &proposal.claim;
    let secret = [
        match &claim.value {
            ClaimValue::Text(text) => Some(text.as_str()),
            _ => None,
        },
        claim.epistemic.condition(),
        claim.slot.as_ref().map(|slot| slot.as_str()),
        Some(claim.subject.key.as_str()),
    ]
    .into_iter()
    .flatten()
    .any(|text| aicortex_gate::secrets::scan(text, &gate.rules().secrets).is_some());
    let floor = case.policy.document().score_floor;
    [
        aicortex_claims::validate(claim, registry).is_err(),
        secret,
        proposal.evidence.is_empty(),
        oracle::unavailable_source(&proposal, &case.context()).is_some(),
        floor.is_some_and(|floor| {
            proposal.evidence.iter().any(
                |evidence| matches!(evidence, Evidence::ModelScore { score, .. } if *score < floor),
            )
        }),
    ]
    .into_iter()
    .filter(|fault| *fault)
    .count()
}

#[derive(Default)]
struct Seen {
    labels: BTreeSet<&'static str>,
    codes: BTreeSet<&'static str>,
    rules: BTreeSet<&'static str>,
    held: BTreeSet<&'static str>,
    multi: usize,
    cases: usize,
}

impl Seen {
    fn record(&mut self, verdict: &ClaimVerdict, faults: usize) {
        self.cases += 1;
        self.labels.insert(verdict.label());
        if faults >= 2 {
            self.multi += 1;
        }
        match verdict {
            ClaimVerdict::Refuse(reason) => {
                self.codes.insert(reason.code());
                if let ClaimReason::PolicyDenied { rule } = reason {
                    self.rules.insert(rule);
                }
            }
            ClaimVerdict::Hold(_, reason) => {
                self.held.insert(reason.code());
            }
            ClaimVerdict::Admit(_) => {}
        }
    }
}

#[test]
fn fr002_claim_walk_matches_the_old_evaluator_over_the_corpus_and_its_variations() {
    let corpus = claim_corpus();
    assert!(corpus.len() >= 30, "only {} fixtures", corpus.len());
    let pools = pools(&corpus);
    let registry = registry();
    let gate = Gate::standard();
    let mut seen = Seen::default();
    let mut check = |case: &ClaimCase, name: &str| {
        let verdict = same_claim(&gate, &registry, case, name);
        seen.record(&verdict, claim_faults(&gate, &registry, case));
    };

    // Every fixture, then every fixture varied along one axis at a time.
    for fixture in &corpus {
        let base = ClaimCase::of(fixture);
        let mut variants = vec![("as recorded".to_owned(), base.clone())];
        let mut vary = |name: String, change: &dyn Fn(&mut ClaimCase)| {
            let mut case = base.clone();
            change(&mut case);
            variants.push((name, case));
        };
        for role in PROPOSERS {
            vary(format!("as {role}"), &|case| case.proposer = role);
        }
        vary("without evidence".into(), &|case| case.evidence.clear());
        for index in 0..base.evidence.len() {
            vary(format!("without evidence {index}"), &|case| {
                case.evidence.remove(index);
            });
        }
        for (index, item) in pools.evidence.iter().enumerate() {
            vary(format!("plus evidence {index}"), &|case| {
                case.evidence.push(item.clone());
            });
        }
        for (index, relations) in pools.relations.iter().enumerate() {
            vary(format!("relations {index}"), &|case| {
                case.relations.clone_from(relations);
            });
        }
        for (index, policy) in pools.policies.iter().enumerate() {
            vary(format!("policy {index}"), &|case| {
                case.policy.clone_from(policy);
            });
        }
        for (index, sources) in pools.sources.iter().enumerate() {
            vary(format!("sources {index}"), &|case| {
                case.sources.clone_from(sources);
            });
        }
        for (index, targets) in pools.targets.iter().enumerate() {
            vary(format!("targets {index}"), &|case| {
                case.targets.clone_from(targets);
            });
        }
        for (name, case) in &variants {
            check(case, &format!("{} {name}", fixture.name));
        }
    }

    // Then every axis at once, sampled with a fixed seed.
    let mut seeded = Seeded(0x0057_AC6A_7E00_0051);
    for sample in 0..20_000 {
        let count = seeded.below(4);
        let evidence = (0..count)
            .map(|_| seeded.pick(&pools.evidence).clone())
            .collect();
        let case = ClaimCase {
            claim: seeded.pick(&pools.claims).clone(),
            proposer: seeded.pick(&PROPOSERS),
            evidence,
            relations: seeded.pick(&pools.relations).clone(),
            policy: seeded.pick(&pools.policies).clone(),
            sources: seeded.pick(&pools.sources).clone(),
            targets: seeded.pick(&pools.targets).clone(),
        };
        check(&case, &format!("sample {sample}"));
    }

    eprintln!(
        "051: {} cases, {} with two or more leading faults",
        seen.cases, seen.multi
    );
    assert_eq!(
        seen.labels,
        BTreeSet::from(["admit", "hold", "refuse"]),
        "a verdict was never reached"
    );
    for code in [
        "unregistered_predicate",
        "invalid_value",
        "secret_detected",
        "source_unavailable",
        "no_provenance",
        "authority_insufficient",
        "below_score_floor",
        "policy_denied",
    ] {
        assert!(seen.codes.contains(code), "no refusal was {code}");
    }
    for rule in [
        "seed_with_user_statement",
        "user_statement_below_user_level",
        "unknown_target",
        "cross_scope_relation",
        "user_supersedes_supplier",
        "self_relation",
    ] {
        assert!(seen.rules.contains(rule), "no refusal named {rule}");
    }
    assert!(
        seen.held.contains("authority_insufficient"),
        "nothing was held"
    );
    assert!(seen.multi >= 1_000, "only {} multi-fault cases", seen.multi);
}
