//! The public host-library contract of spec 053.

#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used
)]

use aicortex_claims::RegistrySnapshot;
use aicortex_store::{
    AICORTEX_MIGRATION_SET_NAME, OperatorPredicateConfig, PredicateRegistrationGrant,
    PredicateRegistrationPlan, PredicateRegistrationRefusal, PredicateRegistryRepo, Registration,
    document_digest, migration_set, migrations,
};
use aicortex_types::PredicateSet;
use rahi_store::TxnBuilder;
use rahi_types::{Sub, UnixSeconds};

const REGISTRIES: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../aicortex-claims/testdata/registries"
);

fn set(name: &str) -> PredicateSet {
    let text = std::fs::read_to_string(format!("{REGISTRIES}/{name}"))
        .expect("the registry fixture reads");
    serde_json::from_str(&text).expect("the registry fixture parses")
}

fn config(set: &PredicateSet, digest: impl Into<String>) -> OperatorPredicateConfig {
    OperatorPredicateConfig::new(
        Sub::new("sub-operator"),
        vec![PredicateRegistrationGrant::new(
            set.namespace().clone(),
            set.version(),
            digest,
        )],
    )
    .expect("one grant is non-empty configuration")
}

#[test]
fn fr001_fr002_the_named_set_is_exact_and_immutable() {
    let set = migration_set().expect("the fixed contract is valid");
    assert_eq!(set.name.as_str(), AICORTEX_MIGRATION_SET_NAME);
    assert_eq!(set.migrations, migrations());
    assert_eq!(
        set.migrations
            .iter()
            .map(|migration| migration.version)
            .collect::<Vec<_>>(),
        (1..=8).collect::<Vec<_>>()
    );
    assert_eq!(set.requires.len(), 2);
    assert_eq!(set.requires[0].set.as_str(), "rahi.receipts");
    assert_eq!(set.requires[0].min_version, 1);
    assert_eq!(set.requires[1].set.as_str(), "rahi.coordination");
    assert_eq!(set.requires[1].min_version, 1);

    let expected = [
        (
            1,
            "rahi-store coordination",
            "sha256:c0ea41566e2727e4476f07054ba0afd7b996121c22fd28872762d65ceb8fbf53",
            true,
        ),
        (
            2,
            "aicortex memory schema",
            "sha256:f5cbf8024fc965e2d62b1eaf212c8bcec2a70baaffd0f1ea4b4f83dba2931c43",
            true,
        ),
        (
            3,
            "aicortex decision digest keys",
            "sha256:4da200dd782858caccd845e30ae44fe28a048018a24c50a356b4f68eab23e93c",
            true,
        ),
        (
            4,
            "aicortex predicate registry",
            "sha256:39c9832a6694bb65a6517c44f92cbc6147b5f1cb70247d1ba690d7575eec85b7",
            true,
        ),
        (
            5,
            "aicortex claim admission",
            "sha256:69781104e88e98e905bb0f0fdef8275116235997cbd91fdb04637bcb88ca605c",
            true,
        ),
        (
            6,
            "aicortex lifecycle columns, source log and erasure journal",
            "sha256:e4c688e65dee32ef6ec68a0643ab10d51199959fcbdb5f2978db2926cf6bb97f",
            true,
        ),
        (
            7,
            "aicortex durable erasure receipts and fingerprint progress",
            "sha256:32bcfdb47aff64bfaa5e8812826c13a56f8f0ea7d88052b947a35e4aab8eb31a",
            false,
        ),
        (
            8,
            "aicortex append-only bitemporal claim history",
            "sha256:16a63aee97b45b82f024bd21e7fb1dbf4ed6519112d633df679ec01244b179ac",
            true,
        ),
    ];
    for (migration, (version, name, checksum, additive)) in set.migrations.iter().zip(expected) {
        assert_eq!(migration.version, version);
        assert_eq!(migration.name, name);
        assert_eq!(migration.checksum(), checksum);
        assert_eq!(migration.additive, additive);
    }
}

#[test]
fn fr004_authority_validation_is_typed_pure_and_idempotent() {
    let travel = set("travel-v1.json");
    let digest = document_digest(&travel).expect("the canonical document digests");
    let authorized = config(&travel, digest.clone());
    let empty = RegistrySnapshot::new();

    let plan = PredicateRegistryRepo::validate_registration(&empty, &travel, &authorized)
        .expect("the exact operator grant authorizes the document");
    assert!(matches!(plan, PredicateRegistrationPlan::Insert(_)));
    let mut txn = TxnBuilder::new();
    assert!(matches!(
        PredicateRegistryRepo::stage_registration(&mut txn, plan, UnixSeconds::new(1_789_041_600)),
        Ok(Registration::Registered(_))
    ));
    assert_eq!(txn.len(), 1);

    let snapshot = RegistrySnapshot::from_sets(vec![travel.clone()]).expect("one legal set");
    let unchanged = PredicateRegistryRepo::validate_registration(&snapshot, &travel, &authorized)
        .expect("the identical version is a no-op");
    let before = txn.len();
    assert!(matches!(
        PredicateRegistryRepo::stage_registration(
            &mut txn,
            unchanged,
            UnixSeconds::new(1_789_041_601)
        ),
        Ok(Registration::Unchanged)
    ));
    assert_eq!(txn.len(), before);

    let absent = OperatorPredicateConfig::new(
        Sub::new("sub-operator"),
        vec![PredicateRegistrationGrant::new(
            aicortex_types::Namespace::new("other").expect("a namespace"),
            1,
            digest.clone(),
        )],
    )
    .expect("one grant");
    assert!(matches!(
        PredicateRegistryRepo::validate_registration(&empty, &travel, &absent),
        Err(PredicateRegistrationRefusal::NotOperatorGranted { .. })
    ));

    let mismatch = config(&travel, format!("sha256:{}", "00".repeat(32)));
    assert!(matches!(
        PredicateRegistryRepo::validate_registration(&empty, &travel, &mismatch),
        Err(PredicateRegistrationRefusal::GrantDigestMismatch { .. })
    ));

    let mut changed: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(format!("{REGISTRIES}/travel-v1.json"))
            .expect("the fixture reads"),
    )
    .expect("the fixture parses");
    changed["predicates"][6]["epistemic"] = serde_json::json!(["asserted", "confirmed"]);
    let changed: PredicateSet = serde_json::from_value(changed).expect("the changed set is legal");
    let changed_config = config(
        &changed,
        document_digest(&changed).expect("the changed document digests"),
    );
    assert!(matches!(
        PredicateRegistryRepo::validate_registration(&snapshot, &changed, &changed_config),
        Err(PredicateRegistrationRefusal::ConflictingVersion { .. })
    ));
}
