#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

mod common;

use std::collections::BTreeMap;

use aicortex_claims::{ClaimHistory, RegistrySnapshot, TxBound, ValidTime};
use aicortex_gate::{
    AdmissionPolicy, AdmittedClaim, ClaimContext, ClaimVerdict, Gate, SourceState,
};
use aicortex_store::{
    Authority, ClaimAppend, ClaimRepo, Eraser, Erasure, Lifecycle, ScopeId, StoredClaim,
};
use aicortex_types::{
    Actor, ActorId, AuthorityLevel, Claim, ClaimId, ClaimParts, ClaimProposal, ClaimValue,
    EpistemicStatus, Evidence, MemoryId, Namespace, PredicateRef, PredicateSet, ProposalId,
    Provenance, Scope, SourceRef, SourceSystem, Stance, SubjectKey, SubjectKind, SubjectRef,
    SupersessionRule, ValidTimeMode,
};
use rahi_store::{Envelope, TxnBuilder, Value};
use rahi_types::{Revision, Sub, UnixSeconds};

fn at(value: u64) -> UnixSeconds {
    UnixSeconds::new(1_800_100_000 + value)
}

fn registry() -> RegistrySnapshot {
    let set: PredicateSet = serde_json::from_str(include_str!(
        "../../aicortex-claims/testdata/registries/travel-v1.json"
    ))
    .unwrap();
    RegistrySnapshot::from_sets([set]).unwrap()
}

fn proposal(id: u128, source: MemoryId, key: &str) -> ClaimProposal {
    let predicate = PredicateRef::parse("travel:booking.locator@1").unwrap();
    let mut provenance = Provenance::captured(
        SourceRef::new(SourceSystem::new("mail").unwrap()),
        at(0),
        at(1),
    );
    provenance.derived_from.push(source);
    ClaimProposal {
        id: ProposalId::from_uuid(uuid::Uuid::from_u128(10_000 + id)),
        claim: Claim::new(ClaimParts {
            id: ClaimId::from_uuid(uuid::Uuid::from_u128(id)),
            scope: common::scope("traveler"),
            subject: SubjectRef {
                namespace: Namespace::new("travel").unwrap(),
                kind: SubjectKind::new("booking").unwrap(),
                key: SubjectKey::new(key).unwrap(),
            },
            predicate,
            value: ClaimValue::Text("PNR-SENSITIVE".to_owned()),
            slot: None,
            epistemic: EpistemicStatus::plain(Stance::Asserted).unwrap(),
            provenance,
        }),
        proposer: Actor::human(ActorId::new("sub-traveler").unwrap()),
        evidence: vec![Evidence::UserStatement {
            by: Sub::new("sub-traveler"),
            authority: AuthorityLevel::UserAsserted,
        }],
        relations: Vec::new(),
        proposed_at: at(2),
    }
}

fn admit(proposal: &ClaimProposal, source: MemoryId) -> AdmittedClaim {
    let context = ClaimContext {
        sources: BTreeMap::from([(source, SourceState::Available)]),
        targets: BTreeMap::new(),
    };
    match Gate::standard().evaluate_claim(
        proposal,
        &registry(),
        &AdmissionPolicy::initial(),
        &context,
    ) {
        ClaimVerdict::Admit(value) => *value,
        other => panic!("claim was not admitted: {other:?}"),
    }
}

fn work(claim: &Claim) -> Envelope {
    Envelope::new(
        "claim",
        Some("sub-traveler".to_owned()),
        claim.id.to_string(),
        Revision::new(1),
    )
}

async fn capture(node: &common::Node, scope: &Scope, text: &str) -> MemoryId {
    let memory = common::memory(scope, text, at(0).get());
    let admitted = common::admit(&memory);
    let mut txn = TxnBuilder::new();
    let captured = Lifecycle::new()
        .capture(&node.handle(), &mut txn, &admitted, &common::work(&memory))
        .await
        .unwrap();
    node.handle().txn(txn.into_statements()).await.unwrap();
    captured.id()
}

async fn append(node: &common::Node, proposal: &ClaimProposal, admitted: &AdmittedClaim) {
    let item = ClaimAppend {
        admitted,
        valid: ValidTime::Timeless,
        source_time: None,
        source_seq: None,
        valid_time_mode: ValidTimeMode::Timeless,
        supersession: SupersessionRule::ExplicitOnly,
        hostile_content: true,
    };
    let mut txn = TxnBuilder::new();
    ClaimRepo::stage_append(
        &mut txn,
        &ClaimHistory::new(),
        &[item],
        &Sub::new("writer"),
        at(3),
        &work(&proposal.claim),
    )
    .unwrap();
    node.handle().txn(txn.into_statements()).await.unwrap();
}

fn authority() -> Authority {
    Authority::of(Sub::new("sub-traveler")).because("user_request")
}

#[derive(serde::Deserialize)]
struct ErasedRow {
    subject_key: String,
    slot_key: Option<String>,
    claim: Option<String>,
    hostile_content: i64,
    erased: i64,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_erasure_without_cascade_marks_and_still_projects() {
    let node = common::node().await;
    let scope = common::scope("traveler");
    let source = capture(&node, &scope, "booking email").await;
    let proposal = proposal(101, source, "booking-one");
    let admitted = admit(&proposal, source);
    append(&node, &proposal, &admitted).await;
    Eraser::new()
        .erase(
            &node.handle(),
            &node.ledger,
            &Erasure::new(&scope, source, &authority(), at(10)),
        )
        .await
        .unwrap();
    let stored = ClaimRepo::get(
        &node.handle(),
        &scope,
        &proposal.claim.subject,
        proposal.claim.id,
    )
    .await
    .unwrap()
    .unwrap();
    let StoredClaim::Live(record) = stored else {
        panic!("non-cascade erased the claim")
    };
    assert!(record.origin_erased);
    let page = ClaimRepo::history(
        &node.handle(),
        &scope,
        &proposal.claim.subject,
        None,
        &TxBound::Sequence(i64::MAX as u64),
        0,
        10,
    )
    .await
    .unwrap();
    assert_eq!(page.history.claims().len(), 1);
    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_erasure_with_cascade_blanks_attributable_content_and_leaves_tombstone() {
    let node = common::node().await;
    let scope = common::scope("traveler");
    let source = capture(&node, &scope, "booking email").await;
    let proposal = proposal(102, source, "booking-two");
    let admitted = admit(&proposal, source);
    append(&node, &proposal, &admitted).await;
    Eraser::new()
        .erase(
            &node.handle(),
            &node.ledger,
            &Erasure::new(&scope, source, &authority(), at(10)).cascading(),
        )
        .await
        .unwrap();
    assert!(
        ClaimRepo::get(
            &node.handle(),
            &scope,
            &proposal.claim.subject,
            proposal.claim.id
        )
        .await
        .unwrap()
        .is_none()
    );
    assert!(matches!(
        ClaimRepo::resolve_tombstone(
            &node.handle(),
            &scope,
            &proposal.claim.subject,
            proposal.claim.id
        )
        .await
        .unwrap(),
        Some(StoredClaim::Tombstone { .. })
    ));
    let rows: Vec<ErasedRow> = node.handle().query_consistent(
        "SELECT subject_key, slot_key, claim, hostile_content, erased FROM claim_history WHERE scope_id = ?1 AND claim_id = ?2",
        vec![Value::from(&ScopeId::of(&scope)), Value::from(proposal.claim.id.to_string())],
    ).await.unwrap();
    assert_eq!(rows[0].subject_key, "");
    assert_eq!(rows[0].slot_key, None);
    assert_eq!(rows[0].claim, None);
    assert_eq!(rows[0].hostile_content, 0);
    assert_eq!(rows[0].erased, 1);
    let page = ClaimRepo::history(
        &node.handle(),
        &scope,
        &proposal.claim.subject,
        None,
        &TxBound::Sequence(i64::MAX as u64),
        0,
        10,
    )
    .await
    .unwrap();
    assert!(page.history.claims().is_empty());
    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn direct_claim_erasure_is_staged_and_scrubs_admission_evidence() {
    let node = common::node().await;
    let scope = common::scope("traveler");
    let source = capture(&node, &scope, "booking email").await;
    let proposal = proposal(103, source, "booking-three");
    let admitted = admit(&proposal, source);
    append(&node, &proposal, &admitted).await;
    let mut txn = TxnBuilder::new();
    ClaimRepo::stage_erase(&mut txn, &scope, &proposal.claim.subject, proposal.claim.id);
    node.handle().txn(txn.into_statements()).await.unwrap();
    assert!(matches!(
        ClaimRepo::resolve_tombstone(
            &node.handle(),
            &scope,
            &proposal.claim.subject,
            proposal.claim.id,
        )
        .await
        .unwrap(),
        Some(StoredClaim::Tombstone { .. })
    ));
    #[derive(serde::Deserialize)]
    struct Admission {
        evidence: String,
        relations: String,
    }
    let rows: Vec<Admission> = node
        .handle()
        .query_consistent(
            "SELECT evidence, relations FROM claim_admission
             WHERE scope_id = ?1 AND claim_id = ?2",
            vec![
                Value::from(&ScopeId::of(&scope)),
                Value::from(proposal.claim.id.to_string()),
            ],
        )
        .await
        .unwrap();
    assert_eq!(rows[0].evidence, "[]");
    assert_eq!(rows[0].relations, "[]");
    node.shutdown().await;
}
