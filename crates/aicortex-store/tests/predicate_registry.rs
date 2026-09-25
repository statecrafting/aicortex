//! The predicate registry against rahi's single-voter harness (spec 050
//! AC-3, B-15, FR-005).
//!
//! Registering a version stages one row in the caller's transaction and
//! hands back the Decision to append; the ledger then holds that Decision
//! naming the namespace, the version, the document digest and the
//! registering subject. Registering the same version again with identical
//! content stages nothing; with different content, or with a changed value
//! type or cardinality, it is refused before anything is staged.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

mod common;

use aicortex_store::{KIND_REGISTER, PredicateRegistryRepo, Registration, document_digest};
use aicortex_types::PredicateSet;
use rahi_ledger::{Decision, DecisionId, DecisionKind, Hash, Ledger, LedgerSigner, Outcome};
use rahi_store::TxnBuilder;
use rahi_types::{Error, UnixSeconds};

const REGISTRIES: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../aicortex-claims/testdata/registries"
);

fn document(name: &str) -> serde_json::Value {
    let text = std::fs::read_to_string(format!("{REGISTRIES}/{name}")).expect("the fixture reads");
    serde_json::from_str(&text).expect("the fixture parses")
}

fn set(document: serde_json::Value) -> PredicateSet {
    serde_json::from_value(document).expect("a legal predicate set")
}

async fn register(
    store: &rahi_store::StoreHandle,
    set: &PredicateSet,
) -> Result<Registration, Error> {
    let snapshot = PredicateRegistryRepo::snapshot(store).await?;
    let mut txn = TxnBuilder::new();
    let registration = PredicateRegistryRepo::stage_register(
        &mut txn,
        &snapshot,
        set,
        &common::sub("registrar"),
        UnixSeconds::new(1_789_041_600),
    )?;
    let statements = txn.into_statements();
    if !statements.is_empty() {
        store.txn(statements).await?;
    }
    Ok(registration)
}

#[tokio::test(flavor = "multi_thread")]
async fn ac003_registration_is_recorded_once_and_immutable() {
    let fixture = common::Fixture::migrated().await;
    let store = fixture.handle();
    let ledger = Ledger::open(
        store.clone(),
        LedgerSigner::from_seed([5u8; 32]),
        Hash::parse(format!("sha256:{}", "cd".repeat(32))).expect("a legal manifest hash"),
    )
    .await
    .expect("a fresh chain opens");

    let before = ledger.records().await.expect("the chain reads").len();
    let v1 = set(document("travel-v1.json"));
    let Registration::Registered(entry) = register(&store, &v1).await.expect("version 1 registers")
    else {
        panic!("version 1 was not new");
    };
    assert_eq!(entry.kind, KIND_REGISTER);
    assert!(!entry.denied);
    assert_eq!(entry.payload["namespace"], "travel");
    assert_eq!(entry.payload["version"], 1);
    assert_eq!(
        entry.payload["digest"],
        document_digest(&v1).expect("a digest")
    );
    assert_eq!(
        entry.payload["registered_by"],
        common::sub("registrar").as_str()
    );
    ledger
        .append(
            Decision::new(
                DecisionId::new("claims.registry.register-travel-1"),
                DecisionKind::new(entry.kind),
                entry.actor.clone(),
                Outcome::Allow,
                entry.reason.clone(),
            )
            .with_payload(entry.payload.clone()),
        )
        .await
        .expect("the Decision appends");
    let records = ledger.records().await.expect("the chain reads");
    assert_eq!(records.len(), before + 1, "one registration, one Decision");
    assert!(ledger.verify().await.is_ok());

    let snapshot = PredicateRegistryRepo::snapshot(&store)
        .await
        .expect("the registry reads");
    assert_eq!(
        snapshot.set(v1.namespace(), 1),
        Some(&v1),
        "the stored document reads back"
    );

    // FR-005: identical content under a registered version is a no-op.
    assert_eq!(
        register(&store, &v1).await.expect("a no-op"),
        Registration::Unchanged
    );

    // FR-005: different content under a registered version is refused.
    let mut different = document("travel-v1.json");
    different["predicates"][6]["epistemic"] = serde_json::json!(["asserted", "confirmed"]);
    match register(&store, &set(different)).await {
        Err(Error::Conflict(_)) => {}
        other => panic!("different content under version 1: {other:?}"),
    }

    // FR-005: a later version may not change a value type or a cardinality.
    let mut retyped = document("travel-v1.json");
    retyped["version"] = serde_json::json!(2);
    retyped["predicates"][0]["value"] = serde_json::json!({ "type": "text" });
    match register(&store, &set(retyped)).await {
        Err(Error::Validation(message)) => {
            assert!(message.contains("value_type_changed"), "{message}")
        }
        other => panic!("a changed value type: {other:?}"),
    }
    let mut recounted = document("travel-v1.json");
    recounted["version"] = serde_json::json!(2);
    recounted["predicates"][1]["cardinality"] = serde_json::json!("one");
    recounted["predicates"][1]
        .as_object_mut()
        .expect("an object")
        .remove("slot");
    match register(&store, &set(recounted)).await {
        Err(Error::Validation(message)) => {
            assert!(message.contains("cardinality_changed"), "{message}")
        }
        other => panic!("a changed cardinality: {other:?}"),
    }

    // An additive version 2 registers beside version 1.
    let v2 = set(document("travel-v2.json"));
    assert!(matches!(
        register(&store, &v2).await,
        Ok(Registration::Registered(_))
    ));
    let snapshot = PredicateRegistryRepo::snapshot(&store)
        .await
        .expect("the registry reads");
    assert_eq!(snapshot.sets().count(), 2);

    // The table's key refuses a second row for a version whatever the rule
    // said: a transaction that stages one fails.
    let mut txn = TxnBuilder::new();
    let empty = aicortex_claims::RegistrySnapshot::new();
    PredicateRegistryRepo::stage_register(
        &mut txn,
        &empty,
        &v1,
        &common::sub("racer"),
        UnixSeconds::new(1_789_041_700),
    )
    .expect("an empty snapshot admits it");
    assert!(
        store.txn(txn.into_statements()).await.is_err(),
        "the primary key holds"
    );

    fixture.shutdown().await;
}
