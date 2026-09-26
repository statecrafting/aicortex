#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

mod common;

use std::collections::BTreeMap;

use aicortex_claims::{ClaimHistory, RegistrySnapshot, Retraction, TxBound, TxStamp, ValidTime};
use aicortex_gate::{AdmissionPolicy, AdmittedClaim, ClaimContext, ClaimVerdict, Gate};
use aicortex_store::{ClaimAppend, ClaimRepo, ScopeId};
use aicortex_types::{
    Actor, ActorId, AuthorityLevel, Claim, ClaimId, ClaimParts, ClaimProposal, ClaimValue,
    EpistemicStatus, Evidence, Namespace, PredicateRef, PredicateSet, ProposalId, Provenance,
    Scope, SourceRef, SourceSystem, Stance, SubjectKey, SubjectKind, SubjectRef, SupersessionRule,
    ValidTimeMode,
};
use rahi_store::{Envelope, Statement, TxnBuilder, Value};
use rahi_types::{Revision, Sub, UnixSeconds};

fn at(value: u64) -> UnixSeconds {
    UnixSeconds::new(1_800_000_000 + value)
}

fn registry() -> RegistrySnapshot {
    let set: PredicateSet = serde_json::from_str(include_str!(
        "../../aicortex-claims/testdata/registries/travel-v1.json"
    ))
    .unwrap();
    RegistrySnapshot::from_sets([set]).unwrap()
}

fn scope() -> Scope {
    Scope::personal(Sub::new("traveler"))
}

fn proposal(id: u128, value: &str, source: Option<aicortex_types::MemoryId>) -> ClaimProposal {
    let predicate = PredicateRef::parse("travel:booking.locator@1").unwrap();
    let mut provenance = Provenance::captured(
        SourceRef::new(SourceSystem::new("mail").unwrap()),
        at(0),
        at(1),
    );
    if let Some(source) = source {
        provenance.derived_from.push(source);
    }
    ClaimProposal {
        id: ProposalId::from_uuid(uuid::Uuid::from_u128(10_000 + id)),
        claim: Claim::new(ClaimParts {
            id: ClaimId::from_uuid(uuid::Uuid::from_u128(id)),
            scope: scope(),
            subject: SubjectRef {
                namespace: Namespace::new("travel").unwrap(),
                kind: SubjectKind::new("booking").unwrap(),
                key: SubjectKey::new("booking-1").unwrap(),
            },
            predicate,
            value: ClaimValue::Text(value.to_owned()),
            slot: None,
            epistemic: EpistemicStatus::plain(Stance::Asserted).unwrap(),
            provenance,
        }),
        proposer: Actor::human(ActorId::new("traveler").unwrap()),
        evidence: vec![Evidence::UserStatement {
            by: Sub::new("traveler"),
            authority: AuthorityLevel::UserAsserted,
        }],
        relations: Vec::new(),
        proposed_at: at(2),
    }
}

fn admit(proposal: &ClaimProposal, source: Option<aicortex_types::MemoryId>) -> AdmittedClaim {
    let context = ClaimContext {
        sources: source
            .map(|id| BTreeMap::from([(id, aicortex_gate::SourceState::Available)]))
            .unwrap_or_default(),
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
        Some("traveler".to_owned()),
        claim.id.to_string(),
        Revision::new(1),
    )
}

#[derive(serde::Deserialize)]
struct Count {
    count: i64,
}

#[derive(serde::Deserialize)]
struct Document {
    claim: Option<String>,
}

async fn count(store: &rahi_store::StoreHandle, table: &str, scope: &Scope) -> i64 {
    let rows: Vec<Count> = store
        .query_consistent(
            format!("SELECT count(*) AS count FROM {table} WHERE scope_id = ?1"),
            vec![Value::from(&ScopeId::of(scope))],
        )
        .await
        .unwrap();
    rows[0].count
}

async fn claim_document(store: &rahi_store::StoreHandle, id: ClaimId) -> Option<String> {
    let rows: Vec<Document> = store
        .query_consistent(
            "SELECT claim FROM claim_history WHERE scope_id = ?1 AND claim_id = ?2",
            vec![
                Value::from(&ScopeId::of(&scope())),
                Value::from(id.to_string()),
            ],
        )
        .await
        .unwrap();
    rows.into_iter().next().and_then(|row| row.claim)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn append_stages_history_admission_counter_and_outbox_atomically() {
    let fixture = common::Fixture::migrated().await;
    let store = fixture.handle();
    let proposal = proposal(1, "PNR-ONE", None);
    let admitted = admit(&proposal, None);
    let append = ClaimAppend {
        admitted: &admitted,
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
        &[append],
        &Sub::new("writer"),
        at(3),
        &work(&proposal.claim),
    )
    .unwrap();
    store.txn(txn.into_statements()).await.unwrap();

    assert_eq!(count(&store, "claim_history", &scope()).await, 1);
    assert_eq!(count(&store, "claim_admission", &scope()).await, 1);
    assert_eq!(count(&store, "claim_tx_counter", &scope()).await, 1);
    let subject = proposal.claim.subject.clone();
    let page = ClaimRepo::history(
        &store,
        &scope(),
        &subject,
        None,
        &TxBound::Sequence(10),
        0,
        10,
    )
    .await
    .unwrap();
    assert!(page.history.is_bounded_by(&scope(), &subject));
    assert!(page.history.claims()[&proposal.claim.id].hostile_content);

    let other = SubjectRef {
        key: SubjectKey::new("booking-2").unwrap(),
        ..subject
    };
    assert!(
        ClaimRepo::history(
            &store,
            &scope(),
            &other,
            None,
            &TxBound::Sequence(10),
            0,
            10
        )
        .await
        .unwrap()
        .history
        .claims()
        .is_empty()
    );
    fixture.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_caller_transaction_rolls_every_staged_record_back() {
    let fixture = common::Fixture::migrated().await;
    let store = fixture.handle();
    let proposal = proposal(2, "PNR-TWO", None);
    let admitted = admit(&proposal, None);
    let append = ClaimAppend {
        admitted: &admitted,
        valid: ValidTime::Timeless,
        source_time: None,
        source_seq: None,
        valid_time_mode: ValidTimeMode::Timeless,
        supersession: SupersessionRule::ExplicitOnly,
        hostile_content: false,
    };
    let mut txn = TxnBuilder::new();
    ClaimRepo::stage_append(
        &mut txn,
        &ClaimHistory::new(),
        &[append],
        &Sub::new("writer"),
        at(3),
        &work(&proposal.claim),
    )
    .unwrap();
    txn.push(Statement::new(
        "INSERT INTO table_that_does_not_exist VALUES (1)",
    ));
    assert!(store.txn(txn.into_statements()).await.is_err());
    for table in ["claim_history", "claim_admission", "claim_tx_counter"] {
        assert_eq!(
            count(&store, table, &scope()).await,
            0,
            "{table} survived rollback"
        );
    }
    fixture.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retraction_appends_and_never_changes_the_claim_row() {
    let fixture = common::Fixture::migrated().await;
    let store = fixture.handle();
    let proposal = proposal(3, "PNR-THREE", None);
    let admitted = admit(&proposal, None);
    let append = ClaimAppend {
        admitted: &admitted,
        valid: ValidTime::Timeless,
        source_time: None,
        source_seq: None,
        valid_time_mode: ValidTimeMode::Timeless,
        supersession: SupersessionRule::ExplicitOnly,
        hostile_content: false,
    };
    let mut txn = TxnBuilder::new();
    ClaimRepo::stage_append(
        &mut txn,
        &ClaimHistory::new(),
        &[append],
        &Sub::new("writer"),
        at(3),
        &work(&proposal.claim),
    )
    .unwrap();
    store.txn(txn.into_statements()).await.unwrap();
    let before = claim_document(&store, proposal.claim.id).await;
    let retraction = Retraction {
        target: proposal.claim.id,
        reason: "withdrawn".to_owned(),
        by: Actor::human(ActorId::new("traveler").unwrap()),
        provenance: proposal.claim.provenance.clone(),
        tx: TxStamp {
            seq: 0,
            recorded_at: at(4),
        },
    };
    let mut txn = TxnBuilder::new();
    ClaimRepo::stage_retraction(&mut txn, &scope(), &retraction).unwrap();
    store.txn(txn.into_statements()).await.unwrap();
    let after = claim_document(&store, proposal.claim.id).await;
    assert_eq!(before, after);
    assert_eq!(count(&store, "claim_history", &scope()).await, 1);
    assert_eq!(count(&store, "claim_retraction", &scope()).await, 1);
    fixture.shutdown().await;
}
