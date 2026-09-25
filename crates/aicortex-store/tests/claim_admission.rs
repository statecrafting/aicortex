//! Claim admission against rahi's single-voter harness (spec 051 AC-2).
//!
//! Every proposal goes through the real gate: the only way to an admission
//! record is an `AdmittedClaim`, and the only way to one of those is a
//! verdict. The claim history table the admission is written beside is
//! 052's, so here the admission record and the proposal row stand for the
//! stored claim, and the outbox row for its work.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

mod common;

use std::collections::BTreeMap;

use aicortex_claims::RegistrySnapshot;
use aicortex_gate::{
    AdmissionPolicy, AdmittedClaim, ClaimContext, ClaimVerdict, Gate, KIND_CLAIM_BATCH,
    KIND_CLAIM_CORRECTION, KIND_POLICY_CHANGE, PolicyDocument, SeedAcceptance, SourceState,
    proposal_entry,
};
use aicortex_store::{AdmissionDecisions, ClaimAdmissionRepo, ClaimProposalRepo};
use aicortex_types::{
    Actor, ActorId, AgentOrigin, AuthorityLevel, Claim, ClaimId, ClaimParts, ClaimProposal,
    ClaimValue, ContentDigest, DecisionRef, EpistemicStatus, Evidence, ExtractorVersion, Hex,
    MemoryId, PartLocator, PredicateRef, PredicateSet, ProposalId, ProposedRelation, Provenance,
    RelationKind, Scope, SeedRef, SeedSet, SlotKey, SourceRef, SourceSpan, SourceSystem, SpanRange,
    SpanUnit, Stance, SubjectKey, SubjectKind, SubjectRef,
};
use rahi_ledger::{Decision, DecisionId, DecisionKind, Hash, Ledger, LedgerSigner, Outcome};
use rahi_store::{Envelope, Outbox, Statement, StoreHandle, TxnBuilder, Value};
use rahi_types::{Revision, Sub, UnixSeconds};
use serde::Deserialize;

const OWNER: &str = "sub-traveler";
const SOURCE: &str = "019c4f00-0000-7000-8000-0000000000a1";

fn registry() -> RegistrySnapshot {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../aicortex-claims/testdata/registries/travel-v1.json"
    );
    let set: PredicateSet = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    RegistrySnapshot::from_sets([set]).unwrap()
}

fn scope() -> Scope {
    Scope::personal(Sub::new(OWNER))
}

fn owner() -> Actor {
    Actor::human(ActorId::new(OWNER).unwrap())
}

fn agent() -> Actor {
    Actor::agent(ActorId::new("extractor").unwrap(), AgentOrigin::default())
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

fn at(offset: u64) -> UnixSeconds {
    UnixSeconds::new(1_789_041_600 + offset)
}

/// A seat preference, `14C` or whatever `seat` says.
fn seat(seat: &str, derived: bool) -> Claim {
    claim(
        "travel:segment.seat@1",
        ClaimValue::Text(seat.to_owned()),
        Some("traveler-1"),
        Stance::Requested,
        derived,
    )
}

/// A departure time taken from a carrier's message.
fn departure(local: &str) -> Claim {
    let value = serde_json::from_value(serde_json::json!({
        "type": "datetime",
        "datetime": { "local": local, "precision": "minute",
                      "zone": { "kind": "iana", "name": "Europe/Warsaw" } }
    }))
    .unwrap();
    claim(
        "travel:segment.departure@1",
        value,
        None,
        Stance::Asserted,
        true,
    )
}

fn claim(
    predicate: &str,
    value: ClaimValue,
    slot: Option<&str>,
    stance: Stance,
    derived: bool,
) -> Claim {
    let predicate = PredicateRef::parse(predicate).unwrap();
    let mut provenance = Provenance::captured(
        SourceRef::new(SourceSystem::new("travel-ingest").unwrap()),
        at(0),
        at(0),
    );
    if derived {
        provenance.derived_from = vec![MemoryId::parse(SOURCE).unwrap()];
        provenance = provenance.with_spans(vec![span()]);
    }
    Claim::new(ClaimParts {
        id: ClaimId::now_v7(),
        scope: scope(),
        subject: SubjectRef {
            namespace: predicate.namespace.clone(),
            kind: SubjectKind::new("segment").unwrap(),
            key: SubjectKey::new("LO281-2026-10-03").unwrap(),
        },
        predicate,
        value,
        slot: slot.map(|slot| SlotKey::new(slot).unwrap()),
        epistemic: EpistemicStatus::plain(stance).unwrap(),
        provenance,
    })
}

fn statement(level: AuthorityLevel) -> Evidence {
    Evidence::UserStatement {
        by: Sub::new(OWNER),
        authority: level,
    }
}

fn verified() -> Evidence {
    Evidence::SpanVerified {
        span: span(),
        method: ExtractorVersion::new("iata-itinerary", "1.0.0").unwrap(),
    }
}

fn review() -> Evidence {
    Evidence::ReviewApproval {
        by: Sub::new("sub-reviewer"),
        decision: DecisionRef::new("claims.review-0001").unwrap(),
    }
}

fn proposal(
    claim: Claim,
    proposer: Actor,
    evidence: Vec<Evidence>,
    relations: Vec<ProposedRelation>,
) -> ClaimProposal {
    ClaimProposal {
        id: ProposalId::now_v7(),
        claim,
        proposer,
        evidence,
        relations,
        proposed_at: at(60),
    }
}

async fn context(store: &StoreHandle, proposal: &ClaimProposal) -> ClaimContext {
    let targets = ClaimAdmissionRepo::targets(
        store,
        &proposal.claim.scope,
        proposal.relations.iter().map(|relation| relation.to),
    )
    .await
    .unwrap();
    ClaimContext {
        sources: BTreeMap::from([(MemoryId::parse(SOURCE).unwrap(), SourceState::Available)]),
        targets,
    }
}

async fn judge(
    store: &StoreHandle,
    proposal: &ClaimProposal,
    policy: &AdmissionPolicy,
) -> ClaimVerdict {
    let context = context(store, proposal).await;
    Gate::standard().evaluate_claim(proposal, &registry(), policy, &context)
}

fn work(claim: &Claim) -> Envelope {
    Envelope::new(
        "claim",
        Some(OWNER.to_owned()),
        claim.id.to_string(),
        Revision::new(1),
    )
}

/// Append the proposals and admit the batch in one transaction, as a writer
/// would, with the outbox work beside it.
async fn admit(
    store: &StoreHandle,
    proposals: &[&ClaimProposal],
    batch: &[AdmittedClaim],
) -> AdmissionDecisions {
    let mut txn = TxnBuilder::new();
    for proposal in proposals {
        ClaimProposalRepo::stage_append(&mut txn, proposal).unwrap();
    }
    let decisions =
        ClaimAdmissionRepo::stage_admit(&mut txn, batch, &Sub::new("sub-writer"), at(120)).unwrap();
    for admitted in batch {
        Outbox::stage(&mut txn, &work(admitted.claim()));
    }
    store.txn(txn.into_statements()).await.unwrap();
    decisions
}

fn admitted(verdict: ClaimVerdict) -> AdmittedClaim {
    match verdict {
        ClaimVerdict::Admit(admitted) => *admitted,
        other => panic!("not admitted: {other:?}"),
    }
}

async fn ledger(store: &StoreHandle) -> Ledger {
    Ledger::open(
        store.clone(),
        LedgerSigner::from_seed([7u8; 32]),
        Hash::parse(format!("sha256:{}", "ef".repeat(32))).unwrap(),
    )
    .await
    .unwrap()
}

async fn append(ledger: &Ledger, entry: &aicortex_gate::LedgerEntry, n: usize) {
    let outcome = if entry.denied {
        Outcome::Deny
    } else {
        Outcome::Allow
    };
    ledger
        .append(
            Decision::new(
                DecisionId::new(format!("{}-{n}", entry.kind)),
                DecisionKind::new(entry.kind),
                entry.actor.clone(),
                outcome,
                entry.reason.clone(),
            )
            .with_payload(entry.payload.clone()),
        )
        .await
        .unwrap();
}

/// Every column of the admission and proposal rows of one claim, as stored.
async fn rows_of(store: &StoreHandle, claim: ClaimId) -> Vec<BTreeMap<String, serde_json::Value>> {
    let scope_id = aicortex_store::ScopeId::of(&scope());
    let mut rows: Vec<BTreeMap<String, serde_json::Value>> = store
        .query_consistent(
            "SELECT * FROM claim_admission WHERE scope_id = $1 AND claim_id = $2",
            vec![Value::from(&scope_id), Value::from(claim.to_string())],
        )
        .await
        .unwrap();
    let proposals: Vec<BTreeMap<String, serde_json::Value>> = store
        .query_consistent(
            "SELECT * FROM claim_proposal WHERE scope_id = $1 AND claim_id = $2",
            vec![Value::from(&scope_id), Value::from(claim.to_string())],
        )
        .await
        .unwrap();
    rows.extend(proposals);
    assert_eq!(rows.len(), 2, "one admission and one proposal row");
    rows
}

#[derive(Debug, Deserialize)]
struct Count {
    count: i64,
}

async fn count(store: &StoreHandle, table: &str) -> i64 {
    let counted: Vec<Count> = store
        .query_consistent(format!("SELECT count(*) AS count FROM {table}"), vec![])
        .await
        .unwrap();
    counted.first().map(|row| row.count).unwrap_or_default()
}

/// FR-005: a traveler's correction of their own seat preference supersedes
/// it without a review; the earlier rows are byte-identical; a later model
/// extraction of the old value is held.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr005_an_own_correction_supersedes_and_an_extraction_is_held() {
    let fixture = common::Fixture::migrated().await;
    let store = fixture.handle();
    let policy = AdmissionPolicy::initial();

    let first = proposal(
        seat("14C", false),
        owner(),
        vec![statement(AuthorityLevel::UserAsserted)],
        vec![],
    );
    let first_admitted = admitted(judge(&store, &first, &policy).await);
    assert_eq!(first_admitted.authority(), AuthorityLevel::UserAsserted);
    admit(&store, &[&first], &[first_admitted]).await;
    let before = rows_of(&store, first.claim.id).await;

    let correction = proposal(
        seat("2A", false),
        owner(),
        vec![statement(AuthorityLevel::UserCorrected)],
        vec![ProposedRelation {
            kind: RelationKind::Supersedes,
            to: first.claim.id,
        }],
    );
    let corrected = admitted(judge(&store, &correction, &policy).await);
    assert_eq!(corrected.authority(), AuthorityLevel::UserCorrected);
    assert!(corrected.conflicts().is_empty());
    let decisions = admit(&store, &[&correction], &[corrected]).await;
    assert_eq!(decisions.corrections.len(), 1);
    assert_eq!(decisions.corrections[0].kind, KIND_CLAIM_CORRECTION);
    assert_eq!(
        rows_of(&store, first.claim.id).await,
        before,
        "the earlier rows changed"
    );

    let record = ClaimAdmissionRepo::record(&store, &scope(), correction.claim.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(record.corrects, [first.claim.id]);
    assert_eq!(record.relations.len(), 1);
    assert_eq!(record.relations[0].kind(), RelationKind::Supersedes);

    let extraction = proposal(
        seat("14C", true),
        agent(),
        vec![
            Evidence::SourceSpan { span: span() },
            Evidence::ModelScore {
                model: ExtractorVersion::new("extractor", "2026-09").unwrap(),
                score: aicortex_types::Score::new(10_000).unwrap(),
                label: "seat".to_owned(),
            },
        ],
        vec![ProposedRelation {
            kind: RelationKind::Supersedes,
            to: correction.claim.id,
        }],
    );
    let verdict = judge(&store, &extraction, &policy).await;
    assert_eq!(verdict.label(), "hold", "{verdict:?}");

    fixture.shutdown().await;
}

/// FR-010: a correction of a carrier's departure time is kept beside it as a
/// conflict; the supplier rows are byte-identical; a `supersedes` over it is
/// refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr010_a_correction_of_a_supplier_claim_is_kept_in_conflict() {
    let fixture = common::Fixture::migrated().await;
    let store = fixture.handle();
    let policy = AdmissionPolicy::initial();

    let carrier = proposal(
        departure("2026-10-03T14:05"),
        agent(),
        vec![verified(), review()],
        vec![],
    );
    let supplier = admitted(judge(&store, &carrier, &policy).await);
    assert_eq!(supplier.authority(), AuthorityLevel::Verified);
    admit(&store, &[&carrier], &[supplier]).await;
    let before = rows_of(&store, carrier.claim.id).await;

    let mut own = departure("2026-10-03T15:05");
    own.provenance.derived_from.clear();
    own.provenance.spans.clear();
    let correction = proposal(
        own.clone(),
        owner(),
        vec![statement(AuthorityLevel::UserCorrected)],
        vec![ProposedRelation {
            kind: RelationKind::Contradicts,
            to: carrier.claim.id,
        }],
    );
    let corrected = admitted(judge(&store, &correction, &policy).await);
    assert_eq!(corrected.authority(), AuthorityLevel::UserCorrected);
    assert_eq!(corrected.conflicts(), [carrier.claim.id]);
    let decisions = admit(&store, &[&correction], &[corrected]).await;
    assert_eq!(
        rows_of(&store, carrier.claim.id).await,
        before,
        "the supplier rows changed"
    );

    // The review item: the correction's Decision names the conflict for 023.
    let raised = &decisions.corrections[0].payload;
    assert_eq!(raised["conflicts"][0], carrier.claim.id.to_string());
    let record = ClaimAdmissionRepo::record(&store, &scope(), correction.claim.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(record.conflicts, [carrier.claim.id]);
    assert!(
        ClaimAdmissionRepo::record(&store, &scope(), carrier.claim.id)
            .await
            .unwrap()
            .is_some(),
        "both remain"
    );

    let mut again = own;
    again.id = ClaimId::now_v7();
    let overwrite = proposal(
        again,
        owner(),
        vec![statement(AuthorityLevel::UserCorrected)],
        vec![ProposedRelation {
            kind: RelationKind::Supersedes,
            to: carrier.claim.id,
        }],
    );
    let verdict = judge(&store, &overwrite, &policy).await;
    assert_eq!(
        verdict.reason().map(|reason| reason.code()),
        Some("policy_denied"),
        "{verdict:?}"
    );

    fixture.shutdown().await;
}

/// FR-007: re-running the gate over a stored proposal under the policy
/// version its admission record names reproduces the verdict and level.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr007_an_admission_is_reproducible_from_its_record() {
    let fixture = common::Fixture::migrated().await;
    let store = fixture.handle();
    let ledger = ledger(&store).await;

    let seed = SeedRef {
        set: SeedSet::new("starter-profile").unwrap(),
        version: 1,
    };
    let policy = AdmissionPolicy::new(PolicyDocument {
        id: "travel.admission".to_owned(),
        version: 2,
        score_floor: None,
        ceilings: Vec::new(),
        qualified: Vec::new(),
        seeds: vec![SeedAcceptance {
            seed: seed.clone(),
            decision: DecisionRef::new("owner.seed-2026-09-25").unwrap(),
        }],
    })
    .unwrap();
    let mut txn = TxnBuilder::new();
    let entry =
        ClaimAdmissionRepo::stage_policy(&mut txn, &policy, &Sub::new("sub-operator"), at(1))
            .unwrap();
    store.txn(txn.into_statements()).await.unwrap();
    assert_eq!(entry.kind, KIND_POLICY_CHANGE);
    append(&ledger, &entry, 0).await;

    let seeded = proposal(
        seat("14C", false),
        Actor::system(ActorId::new("seeder").unwrap()),
        vec![Evidence::OperatorSeed {
            by: Sub::new("sub-operator"),
            seed,
        }],
        vec![],
    );
    let verdict = judge(&store, &seeded, &policy).await;
    let first = admitted(verdict.clone());
    assert_eq!(first.authority(), AuthorityLevel::Extracted);
    admit(&store, &[&seeded], &[first]).await;

    let record = ClaimAdmissionRepo::record(&store, &scope(), seeded.claim.id)
        .await
        .unwrap()
        .unwrap();
    let stored_policy = ClaimAdmissionRepo::policy(&store, &record.policy)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored_policy, policy);
    let stored = ClaimProposalRepo::get(&store, &scope(), record.proposal_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored, seeded);
    let again = judge(&store, &stored, &stored_policy).await;
    assert_eq!(again, verdict);
    assert_eq!(record.authority, AuthorityLevel::Extracted);
    assert_eq!(record.verdict, "admit");

    // A policy version is immutable: registering it twice fails.
    let mut txn = TxnBuilder::new();
    ClaimAdmissionRepo::stage_policy(&mut txn, &policy, &Sub::new("sub-operator"), at(2)).unwrap();
    assert!(store.txn(txn.into_statements()).await.is_err());

    fixture.shutdown().await;
}

/// FR-008: a failure on the last statement of an admission leaves no
/// proposal, admission record, or outbox row.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr008_a_failed_admission_leaves_nothing() {
    let fixture = common::Fixture::migrated().await;
    let store = fixture.handle();
    let policy = AdmissionPolicy::initial();

    let offered = proposal(
        seat("14C", false),
        owner(),
        vec![statement(AuthorityLevel::UserAsserted)],
        vec![],
    );
    let batch = [admitted(judge(&store, &offered, &policy).await)];
    let before = (
        count(&store, "claim_proposal").await,
        count(&store, "claim_admission").await,
        count(&store, "outbox").await,
    );

    let mut txn = TxnBuilder::new();
    ClaimProposalRepo::stage_append(&mut txn, &offered).unwrap();
    ClaimAdmissionRepo::stage_admit(&mut txn, &batch, &Sub::new("sub-writer"), at(120)).unwrap();
    Outbox::stage(&mut txn, &work(&offered.claim));
    txn.push(Statement::with_params(
        "INSERT INTO claim_admission (claim_id) VALUES ($1)",
        vec![Value::from("this row has no scope and cannot land")],
    ));
    assert!(store.txn(txn.into_statements()).await.is_err());
    assert_eq!(
        (
            count(&store, "claim_proposal").await,
            count(&store, "claim_admission").await,
            count(&store, "outbox").await,
        ),
        before,
        "the rolled-back admission left a trace"
    );

    fixture.shutdown().await;
}

/// FR-009: a reviewed, span-verified proposal is admitted at `verified`, and
/// three admitted in one transaction append exactly one batch Decision
/// naming the three ids and no value. A hold appends its own.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr009_one_transaction_one_batch_decision() {
    let fixture = common::Fixture::migrated().await;
    let store = fixture.handle();
    let ledger = ledger(&store).await;
    let policy = AdmissionPolicy::initial();

    let held = proposal(
        departure("2026-10-03T14:05"),
        agent(),
        vec![verified()],
        vec![],
    );
    let verdict = judge(&store, &held, &policy).await;
    assert_eq!(verdict.label(), "hold");
    let entry = proposal_entry(
        &verdict,
        &held,
        &policy.reference(),
        &Sub::new("sub-writer"),
        None,
    )
    .unwrap();
    let start = ledger.records().await.unwrap().len();
    append(&ledger, &entry, 0).await;

    let proposals: Vec<ClaimProposal> = ["14:05", "14:10", "14:15"]
        .into_iter()
        .map(|time| {
            proposal(
                departure(&format!("2026-10-03T{time}")),
                agent(),
                vec![verified(), review()],
                vec![],
            )
        })
        .collect();
    let mut batch = Vec::new();
    for offered in &proposals {
        let admitted = admitted(judge(&store, offered, &policy).await);
        assert_eq!(admitted.authority(), AuthorityLevel::Verified);
        batch.push(admitted);
    }
    let decisions = admit(&store, &proposals.iter().collect::<Vec<_>>(), &batch).await;
    assert!(decisions.corrections.is_empty());
    append(&ledger, &decisions.batch, 1).await;

    let records = ledger.records().await.unwrap();
    assert_eq!(records.len(), start + 2, "one hold, one batch");
    let batch_entry = &decisions.batch;
    assert_eq!(batch_entry.kind, KIND_CLAIM_BATCH);
    let ids: Vec<String> = proposals
        .iter()
        .map(|offered| offered.claim.id.to_string())
        .collect();
    assert_eq!(batch_entry.payload["claim_ids"], serde_json::json!(ids));
    assert_eq!(batch_entry.payload["count"], 3);
    let written = batch_entry.payload.to_string();
    for value in ["2026-10-03T14", "Europe/Warsaw"] {
        assert!(
            !written.contains(value),
            "the batch Decision carries {value}"
        );
    }
    assert!(ledger.verify().await.is_ok());
    assert_eq!(count(&store, "claim_admission").await, 3);

    fixture.shutdown().await;
}
