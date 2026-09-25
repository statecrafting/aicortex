//! Claim admission (spec 051 AC-1): the fixture corpus of
//! `testdata/claims/`, the score-perturbation test of B-6, the refusal that
//! carries no value, and the batch Decision.
//!
//! A fixture is compact: the claim as in `aicortex-claims`' cases, the
//! proposer by role, the evidence as its wire form with placeholders
//! (`$SPAN`, `$OWNER`, `$STRANGER`, `$OPERATOR`, `$REVIEWER`, `$SOURCE`,
//! `$TARGET`) that the loader substitutes before deserializing.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::collections::BTreeMap;
use std::path::PathBuf;

use aicortex_claims::RegistrySnapshot;
use aicortex_gate::{
    AdmissionPolicy, ClaimContext, ClaimVerdict, Gate, PolicyDocument, SourceState, TargetFacts,
    batch_entry, proposal_entry,
};
use aicortex_types::{
    Actor, ActorId, AgentOrigin, AuthorityLevel, Claim, ClaimId, ClaimParts, ClaimProposal,
    ClaimValue, ContentDigest, EpistemicStatus, Evidence, Hex, MemoryId, PartLocator, PredicateRef,
    PredicateSet, ProposalId, ProposedRelation, Provenance, Scope, Score, SlotKey, SourceRef,
    SourceSpan, SourceSystem, SpanRange, SpanUnit, SubjectKey, SubjectKind, SubjectRef,
};
use rahi_types::{Sub, UnixSeconds};
use serde::Deserialize;

const OWNER: &str = "sub-owner";
const STRANGER: &str = "sub-stranger";
const OPERATOR: &str = "sub-operator";
const REVIEWER: &str = "sub-reviewer";
const SOURCE: &str = "019c4f00-0000-7000-8000-0000000000a1";
const TARGET: &str = "019c4f00-0000-7000-8000-0000000000d4";
const CLAIM: &str = "019c4f00-0000-7000-8000-0000000000c3";
const PROPOSAL: &str = "019c4f00-0000-7000-8000-0000000000e5";

#[derive(Debug, Deserialize)]
struct CaseSubject {
    kind: String,
    key: String,
}

#[derive(Debug, Deserialize)]
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

#[derive(Debug, Deserialize)]
struct Expect {
    verdict: String,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    authority: Option<AuthorityLevel>,
    #[serde(default)]
    sourcing: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Fixture {
    name: String,
    #[allow(dead_code)]
    why: String,
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
    targets: BTreeMap<ClaimId, Target>,
    expect: Expect,
}

#[derive(Debug, Deserialize)]
struct Target {
    authority: AuthorityLevel,
    sourcing: aicortex_types::Sourcing,
}

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testdata/claims")
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

/// Replace every placeholder string, in values and in object keys.
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

fn corpus() -> Vec<Fixture> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir())
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

fn proposer(role: &str) -> Actor {
    match role {
        "owner" => Actor::human(ActorId::new(OWNER).unwrap()),
        "stranger" => Actor::human(ActorId::new(STRANGER).unwrap()),
        "agent" => Actor::agent(ActorId::new("extractor").unwrap(), AgentOrigin::default()),
        "operator" => Actor::system(ActorId::new("seeder").unwrap()),
        other => panic!("unknown proposer role {other}"),
    }
}

impl Fixture {
    fn claim(&self) -> Claim {
        let case = &self.claim;
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

    fn proposal(&self) -> ClaimProposal {
        ClaimProposal {
            id: ProposalId::parse(PROPOSAL).unwrap(),
            claim: self.claim(),
            proposer: proposer(&self.proposer),
            evidence: self.evidence.clone(),
            relations: self.relations.clone(),
            proposed_at: UnixSeconds::new(1_789_041_660),
        }
    }

    fn policy(&self) -> AdmissionPolicy {
        self.policy
            .clone()
            .map_or_else(AdmissionPolicy::initial, |doc| {
                AdmissionPolicy::new(doc).unwrap()
            })
    }

    fn context(&self) -> ClaimContext {
        let sources = self.sources.clone().unwrap_or_else(|| {
            if self.claim.derived {
                BTreeMap::from([(MemoryId::parse(SOURCE).unwrap(), SourceState::Available)])
            } else {
                BTreeMap::new()
            }
        });
        let targets = self
            .targets
            .iter()
            .map(|(id, target)| {
                (
                    *id,
                    TargetFacts {
                        scope: scope(),
                        authority: target.authority,
                        sourcing: target.sourcing,
                    },
                )
            })
            .collect();
        ClaimContext { sources, targets }
    }

    fn judge(&self, proposal: &ClaimProposal) -> ClaimVerdict {
        Gate::standard().evaluate_claim(proposal, &registry(), &self.policy(), &self.context())
    }
}

/// FR-001: every fixture yields its recorded verdict, reason and authority.
#[test]
fn fr001_every_claim_fixture_yields_its_recorded_verdict() {
    let corpus = corpus();
    assert!(corpus.len() >= 30, "only {} fixtures read", corpus.len());
    for fixture in &corpus {
        let verdict = fixture.judge(&fixture.proposal());
        let expect = &fixture.expect;
        assert_eq!(
            verdict.label(),
            expect.verdict,
            "{}: {verdict:?}",
            fixture.name
        );
        assert_eq!(
            verdict.reason().map(|reason| reason.code()),
            expect.reason.as_deref(),
            "{}: the reason is wrong",
            fixture.name
        );
        let admitted = verdict.admitted();
        assert_eq!(
            admitted.map(|admitted| admitted.authority()),
            expect.authority,
            "{}: the authority is wrong",
            fixture.name
        );
        assert_eq!(
            admitted.map(|admitted| admitted.sourcing().label()),
            expect.sourcing.as_deref(),
            "{}: the sourcing is wrong",
            fixture.name
        );
    }
}

/// Every reason code of B-9 and every verdict is exercised by the corpus.
#[test]
fn fr001_the_corpus_covers_every_reason_and_verdict() {
    let corpus = corpus();
    let seen: Vec<String> = corpus
        .iter()
        .filter_map(|fixture| fixture.expect.reason.clone())
        .collect();
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
        assert!(
            seen.iter().any(|reason| reason == code),
            "no fixture for {code}"
        );
    }
    for level in AuthorityLevel::all() {
        assert!(
            corpus
                .iter()
                .any(|fixture| fixture.expect.authority == Some(*level)),
            "no fixture admits at {level}"
        );
    }
}

/// FR-003, B-6: changing every score changes no authority and no admit
/// versus hold outcome, except a floor refusal.
#[test]
fn fr003_scores_never_change_authority_or_admission() {
    for fixture in corpus() {
        let original = fixture.judge(&fixture.proposal());
        for basis_points in [0, 1, 5_000, 9_999, 10_000] {
            let mut proposal = fixture.proposal();
            for evidence in &mut proposal.evidence {
                if let Evidence::ModelScore { score, .. } = evidence {
                    *score = Score::new(basis_points).unwrap();
                }
            }
            let perturbed = fixture.judge(&proposal);
            let floor = |verdict: &ClaimVerdict| {
                verdict.reason().map(|reason| reason.code()) == Some("below_score_floor")
            };
            if floor(&original) || floor(&perturbed) {
                continue;
            }
            assert_eq!(
                perturbed.label(),
                original.label(),
                "{} at {basis_points}",
                fixture.name
            );
            assert_eq!(
                perturbed.admitted().map(|admitted| admitted.authority()),
                original.admitted().map(|admitted| admitted.authority()),
                "{} at {basis_points}",
                fixture.name
            );
        }
    }
}

/// FR-004: a credential in a `Text` value is refused, and neither the
/// verdict nor its Decision carries any part of it.
#[test]
fn fr004_a_refused_credential_is_not_in_the_decision() {
    const SECRET: &str = "ghp_RyIdydJp2Ys2SJ2jXUBATskPzpR6QTFGlbDtQ57k";
    let fixture = corpus()
        .into_iter()
        .find(|fixture| fixture.name == "refuse-secret-in-text")
        .unwrap();
    let proposal = fixture.proposal();
    let verdict = fixture.judge(&proposal);
    let entry = proposal_entry(
        &verdict,
        &proposal,
        &fixture.policy().reference(),
        &Sub::new(OWNER),
        None,
    )
    .unwrap();
    assert_eq!(entry.kind, aicortex_gate::KIND_CLAIM_REFUSE);
    assert!(entry.denied);
    let written = format!("{} {} {verdict:?}", entry.payload, entry.reason);
    for window in SECRET.as_bytes().windows(8) {
        let piece = std::str::from_utf8(window).unwrap();
        assert!(!written.contains(piece), "the Decision carries {piece:?}");
    }
}

/// FR-009 (the gate's half): three admitted claims make one batch Decision
/// naming three ids and no value.
#[test]
fn fr009_one_batch_decision_names_the_ids_and_no_value() {
    let fixture = corpus()
        .into_iter()
        .find(|fixture| fixture.name == "admit-span-verified-reviewed")
        .unwrap();
    let batch: Vec<_> = (0..3)
        .map(|n| {
            let mut proposal = fixture.proposal();
            proposal.claim.id =
                ClaimId::parse(&format!("019c4f00-0000-7000-8000-00000000100{n}")).unwrap();
            match fixture.judge(&proposal) {
                ClaimVerdict::Admit(admitted) => *admitted,
                other => panic!("not admitted: {other:?}"),
            }
        })
        .collect();
    let entry = batch_entry(&batch, &Sub::new(REVIEWER));
    assert_eq!(entry.kind, aicortex_gate::KIND_CLAIM_BATCH);
    assert_eq!(entry.payload["count"], 3);
    assert_eq!(entry.payload["claim_ids"].as_array().unwrap().len(), 3);
    let written = entry.payload.to_string();
    assert!(!written.contains("2026-10-03T14:05"), "{written}");
    assert!(!written.contains("Europe/Warsaw"), "{written}");
}

/// R-1, B-9: the same inputs always yield the same verdict.
#[test]
fn r1_admission_is_reproducible() {
    for fixture in corpus() {
        let proposal = fixture.proposal();
        assert_eq!(
            fixture.judge(&proposal),
            fixture.judge(&proposal),
            "{}",
            fixture.name
        );
    }
}

/// B-8, FR-010: a correction of a supplier claim is kept in conflict, and
/// relations carry the claim's provenance.
#[test]
fn fr010_a_correction_of_a_supplier_claim_is_a_conflict() {
    let fixture = corpus()
        .into_iter()
        .find(|fixture| fixture.name == "admit-correction-contradicts-supplier")
        .unwrap();
    let verdict = fixture.judge(&fixture.proposal());
    let admitted = verdict.admitted().unwrap();
    let target = ClaimId::parse(TARGET).unwrap();
    assert_eq!(admitted.conflicts(), [target]);
    assert_eq!(admitted.corrects(), [target]);
    assert_eq!(admitted.relations().len(), 1);
    assert_eq!(
        admitted.relations()[0].provenance(),
        &admitted.claim().provenance
    );
}

/// B-12: a policy may lower a ceiling and never raise one, and never
/// qualify a predicate on inference.
#[test]
fn b12_a_policy_cannot_raise_a_ceiling_or_qualify_on_inference() {
    let raise = serde_json::json!({
        "id": "p", "version": 1,
        "ceilings": [{ "evidence": "model_score", "level": "extracted" }]
    });
    assert!(serde_json::from_value::<AdmissionPolicy>(raise).is_err());
    let inference = serde_json::json!({
        "id": "p", "version": 1,
        "qualified": [{ "namespace": "travel", "predicate": "booking.locator",
                        "min_authority": "inferred", "decision": "d" }]
    });
    assert!(serde_json::from_value::<AdmissionPolicy>(inference).is_err());
    let lowered = serde_json::json!({
        "id": "p", "version": 1,
        "ceilings": [{ "evidence": "span_verified", "level": "extracted" }]
    });
    let policy: AdmissionPolicy = serde_json::from_value(lowered).unwrap();
    assert_eq!(
        policy.ceiling(aicortex_types::EvidenceKind::SpanVerified),
        Some(AuthorityLevel::Extracted)
    );
    let round = serde_json::to_value(&policy).unwrap();
    assert_eq!(
        serde_json::from_value::<AdmissionPolicy>(round).unwrap(),
        policy
    );
}
