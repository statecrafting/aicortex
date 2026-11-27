//! The adapter contract, its registration, and what the runner builds around
//! an item (spec 030 B-1, B-4 to B-7, FR-004, FR-005).
//!
//! The registration tests need a manifest and nothing else. The ones that
//! reach the kernel's facade boot a real node, so a refusal is the chassis's
//! and not a stand-in.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

mod common;

use aicortex_gate::Origin;
use aicortex_ingest::mapping::{digest, memory_parts};
use aicortex_ingest::{
    AdapterContext, AdapterError, Candidate, Cursor, IngestPolicy, MAX_CURSOR_BYTES,
    MAX_EXTERNAL_ID_BYTES, PublicPolicy, RawItem, Registry, RunId, SourceId, SourceKind,
    ceiling_covers,
};
use aicortex_types::{ActorKind, MemoryKind, TrustClass};
use common::{Stub, ingest_service, manifest, scope};
use rahi_kernel::{CapabilityKind, Egress, Governed, Kernel};
use rahi_types::{EnvReader, Error, Sub, UnixSeconds};
use std::collections::BTreeMap;

struct Env(BTreeMap<String, String>);

impl EnvReader for Env {
    fn get(&self, key: &str) -> Option<String> {
        self.0.get(key).cloned()
    }
}

fn env(pairs: &[(&str, &str)]) -> Env {
    Env(pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect())
}

#[test]
fn a_host_absent_from_the_manifest_ceiling_fails_at_registration() {
    // FR-004: no fetch is made, no node is open, and the adapter never enters
    // the registry.
    let granted = manifest(&["api.example.com"]);
    let mut registry = Registry::new();

    let outside = Stub::with_hosts("reader", SourceKind::Public, &["pages.example.org"]);
    let error = registry
        .register_under(outside, &granted, &ingest_service())
        .expect_err("a host outside the ceiling is refused");
    assert!(matches!(error, Error::Denied(_)), "{error:?}");
    assert!(error.message().contains("pages.example.org"), "{error}");
    assert!(registry.is_empty(), "a refused adapter is not registered");

    // One host in and one out is still out: the ceiling is held per host.
    let mixed = Stub::with_hosts(
        "mixed",
        SourceKind::Public,
        &["api.example.com", "pages.example.org"],
    );
    assert!(
        registry
            .register_under(mixed, &granted, &ingest_service())
            .is_err()
    );

    let inside = Stub::with_hosts("notes", SourceKind::Authenticated, &["api.example.com"]);
    registry
        .register_under(inside, &granted, &ingest_service())
        .expect("a host inside the ceiling registers");
    assert_eq!(registry.len(), 1);

    // An adapter that reaches nothing needs no grant at all.
    let offline = Stub::new("export", SourceKind::Export, Vec::new());
    registry
        .register_under(offline, &manifest(&[]), &ingest_service())
        .expect("an adapter with no host registers under a closed ceiling");
}

#[test]
fn registration_refuses_a_wildcard_a_malformed_host_and_a_second_name() {
    let granted = manifest(&["api.example.com"]);
    let mut registry = Registry::new();

    let wildcard = Stub::with_hosts("wild", SourceKind::Public, &["*.example.com"]);
    let error = registry
        .register_under(wildcard, &granted, &ingest_service())
        .expect_err("an adapter names concrete hosts");
    assert!(matches!(error, Error::Validation(_)), "{error:?}");

    let upper = Stub::with_hosts("upper", SourceKind::Public, &["API.Example.com"]);
    assert!(matches!(
        registry.register_under(upper, &granted, &ingest_service()),
        Err(Error::Validation(_))
    ));

    let first = Stub::with_hosts("notes", SourceKind::Authenticated, &["api.example.com"]);
    registry
        .register_under(first, &granted, &ingest_service())
        .unwrap();
    let again = Stub::with_hosts("notes", SourceKind::Authenticated, &["api.example.com"]);
    assert!(matches!(
        registry.register_under(again, &granted, &ingest_service()),
        Err(Error::Validation(_))
    ));
}

#[test]
fn the_registry_describes_sources_and_finds_a_narrowed_ceiling() {
    let granted = manifest(&["api.example.com"]);
    let mut registry = Registry::new();
    registry
        .register_under(
            Stub::with_hosts("notes", SourceKind::Authenticated, &["api.example.com"]),
            &granted,
            &ingest_service(),
        )
        .unwrap();
    registry
        .register_under(
            Stub::new("export", SourceKind::Export, Vec::new()),
            &granted,
            &ingest_service(),
        )
        .unwrap();

    let described = registry.describe();
    let names: Vec<_> = described.iter().map(|source| source.id.as_str()).collect();
    assert_eq!(names, ["export", "notes"], "in name order");
    assert_eq!(described[1].hosts, ["api.example.com"]);
    assert_eq!(described[1].kind, "authenticated");
    assert!(described[0].hosts.is_empty());

    assert!(
        registry.uncovered(&granted, &ingest_service()).is_empty(),
        "a ceiling that still covers every host reports nothing"
    );
    let narrowed = manifest(&[]);
    assert_eq!(
        registry.uncovered(&narrowed, &ingest_service()),
        [("notes".to_owned(), "api.example.com".to_owned())]
    );
    assert!(ceiling_covers(
        &granted,
        &ingest_service(),
        "api.example.com"
    ));
    assert!(!ceiling_covers(
        &granted,
        &ingest_service(),
        "pages.example.org"
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_adapter_reaches_a_host_only_through_the_kernels_facade() {
    // B-6: the permit is the kernel's. Two checks stand in front of it: the
    // host is one the adapter declared, and the kernel admits it.
    let granted = manifest(&["api.example.com", "other.example.com"]);
    let node = common::node_for(&granted).await;
    let kernel = Kernel::boot(granted, node.handle(), node.ledger.clone())
        .await
        .unwrap();
    let egress = Governed::new(
        &kernel,
        aicortex_ingest::INGEST_SERVICE,
        CapabilityKind::HttpEgress,
        "*",
        Egress,
    )
    .unwrap();

    let stub = Stub::with_hosts("notes", SourceKind::Authenticated, &["api.example.com"]);
    let mut registry = Registry::new();
    registry
        .register(stub, &egress)
        .expect("a covered host registers");
    let registered = registry
        .get(&SourceId::new("notes").unwrap())
        .unwrap()
        .clone();

    let context = |facade: Option<Governed<Egress>>| {
        AdapterContext::new(
            serde_json::Value::Null,
            10,
            registered.hosts(),
            facade,
            Sub::new("alice"),
        )
    };

    let permit = context(Some(egress.clone()))
        .permit("api.example.com")
        .await
        .expect("a declared and granted host is admitted");
    assert_eq!(permit.host(), "api.example.com");

    // Granted by the manifest, never declared by the adapter: refused before
    // the kernel is asked.
    let undeclared = context(Some(egress.clone()))
        .permit("other.example.com")
        .await
        .expect_err("an undeclared host is refused");
    assert!(
        matches!(undeclared, AdapterError::Fatal(_)),
        "{undeclared:?}"
    );

    // A cell with no facade has no network at all.
    let none = context(None).permit("api.example.com").await;
    assert!(matches!(none, Err(AdapterError::Fatal(_))), "{none:?}");

    node.shutdown().await;
}

#[test]
fn the_runner_builds_provenance_and_the_actor_from_what_it_knows() {
    // B-5: the source system is the registration's, the external id and the
    // timestamp are the item's, and the actor is the run.
    let source = SourceId::new("notes").unwrap();
    let run = RunId::mint();
    let item = RawItem::new(
        "ext-42",
        UnixSeconds::new(1_650_000_000),
        serde_json::json!({}),
    )
    .with_locator("fixture://notes/42");
    let candidate = Candidate::new(MemoryKind::Observation, "a note").with_title("A title");
    let now = UnixSeconds::new(1_800_000_000);

    let parts = memory_parts(
        &scope("alice"),
        &source,
        &run,
        &item,
        &candidate,
        TrustClass::Assertion,
        now,
    )
    .unwrap();

    let provenance = &parts.provenance;
    assert_eq!(provenance.source.system.as_str(), "notes");
    assert_eq!(provenance.source.external_id.as_deref(), Some("ext-42"));
    assert_eq!(
        provenance.source.locator.as_deref(),
        Some("fixture://notes/42")
    );
    assert_eq!(provenance.captured_at, UnixSeconds::new(1_650_000_000));
    assert_eq!(provenance.ingested_at, now);
    assert!(matches!(parts.actor.kind, ActorKind::Import));
    assert_eq!(parts.actor.id.as_str(), run.as_str());
    assert_eq!(parts.actor.label.as_deref(), Some("notes"));
    assert_eq!(parts.body.text, "a note");
    assert_eq!(parts.body.title.as_deref(), Some("A title"));
    assert_eq!(parts.trust, TrustClass::Assertion);
    assert_eq!(parts.created, now);
}

#[test]
fn the_digest_tells_a_changed_item_from_an_unchanged_one() {
    let base = Candidate::new(MemoryKind::Observation, "same words");
    assert_eq!(digest(&base), digest(&base.clone()));
    assert_ne!(
        digest(&base),
        digest(&Candidate::new(MemoryKind::Observation, "other words"))
    );
    assert_ne!(
        digest(&base),
        digest(&Candidate::new(MemoryKind::Fact, "same words"))
    );
    assert_ne!(digest(&base), digest(&base.clone().with_title("titled")));
    // Length prefixes keep the boundary between title and text from moving.
    assert_ne!(
        digest(&Candidate::new(MemoryKind::Observation, "bc").with_title("a")),
        digest(&Candidate::new(MemoryKind::Observation, "c").with_title("ab")),
    );
}

#[test]
fn trust_follows_the_kind_of_the_source() {
    // B-7, as a table. The gate makes the final call (013 B-7); this is the
    // path each kind takes to it.
    let default = IngestPolicy::default();
    assert_eq!(
        default.admission_for(SourceKind::Authenticated),
        (Origin::RegisteredAdapter, TrustClass::Evidence)
    );
    assert_eq!(
        default.admission_for(SourceKind::Export),
        (Origin::RegisteredAdapter, TrustClass::Assertion)
    );
    assert_eq!(
        default.admission_for(SourceKind::Public),
        (Origin::Asserted, TrustClass::Assertion),
        "a public source is quarantined unless the deployment says otherwise"
    );
    let open = IngestPolicy {
        public: PublicPolicy::Admit,
    };
    assert_eq!(
        open.admission_for(SourceKind::Public),
        (Origin::RegisteredAdapter, TrustClass::Assertion)
    );
    for kind in SourceKind::ALL {
        for policy in [default, open] {
            assert!(
                policy.admission_for(kind).1.label() != "instruction",
                "no source produces instruction grade"
            );
        }
    }
}

#[test]
fn the_deployments_policy_is_read_from_its_environment_and_a_typo_is_refused() {
    let key = IngestPolicy::PUBLIC_ORIGIN_ENV;
    assert_eq!(
        IngestPolicy::from_env(&env(&[])).unwrap().public,
        PublicPolicy::Quarantine
    );
    assert_eq!(
        IngestPolicy::from_env(&env(&[(key, "quarantine")]))
            .unwrap()
            .public,
        PublicPolicy::Quarantine
    );
    assert_eq!(
        IngestPolicy::from_env(&env(&[(key, "admit")]))
            .unwrap()
            .public,
        PublicPolicy::Admit
    );
    for typo in ["Admit", "true", "", "admit "] {
        assert!(
            matches!(
                IngestPolicy::from_env(&env(&[(key, typo)])),
                Err(Error::Config(_))
            ),
            "{typo:?} must not decide whether a fetched page is trusted"
        );
    }
}

#[test]
fn what_an_adapter_returns_is_bounded_before_it_is_used() {
    let item =
        |external_id: &str| RawItem::new(external_id, UnixSeconds::new(1), serde_json::Value::Null);
    assert!(item("ext-1").validate().is_ok());
    for bad in [
        "",
        " padded",
        "trailing ",
        "tab\there",
        "nul\0here",
        "new\nline",
    ] {
        assert!(item(bad).validate().is_err(), "{bad:?}");
    }
    assert!(item(&"x".repeat(MAX_EXTERNAL_ID_BYTES)).validate().is_ok());
    assert!(
        item(&"x".repeat(MAX_EXTERNAL_ID_BYTES + 1))
            .validate()
            .is_err()
    );
    assert!(item("ok").with_locator("bad\nlocator").validate().is_err());
    assert!(item("ok").with_locator("").validate().is_err());

    assert!(Cursor::new("12").is_ok());
    assert!(Cursor::new("").is_err());
    assert!(Cursor::new("a\nb").is_err());
    assert!(Cursor::new("c".repeat(MAX_CURSOR_BYTES)).is_ok());
    assert!(Cursor::new("c".repeat(MAX_CURSOR_BYTES + 1)).is_err());

    assert!(SourceId::new("chat.export-v2").is_ok());
    for bad in ["", "Upper", "has space", "slash/inside"] {
        assert!(SourceId::new(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn run_ids_are_read_back_strictly() {
    let id = RunId::mint();
    assert_eq!(RunId::parse(id.as_str()).unwrap(), id);
    for bad in [
        "",
        "run-",
        "run-zzz",
        "memory-0",
        &id.as_str().to_uppercase(),
        &format!("{} ", id.as_str()),
    ] {
        assert!(RunId::parse(bad).is_err(), "{bad:?}");
    }
}
