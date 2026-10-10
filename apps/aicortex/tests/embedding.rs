//! The embedding pipeline at the application boundary (spec 015 B-2, B-3,
//! AC-2, FR-003, FR-005).
//!
//! The library crate tests cover the worker, the registry, and the report
//! readers against a hand-built schema. These tests run the application
//! seams the chassis calls, against the cell's real migrations:
//!
//! - the preflight check (`app.embedding`): the named capability failure,
//!   the AC-2 report lines, and the dead-letter warning;
//! - the managed services: which are mounted, that the worker embeds a
//!   staged memory once a model is activated after boot, that the gauges
//!   follow the queue, and that both services return `Ok` on shutdown and
//!   never before it.

#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used
)]

use std::collections::BTreeMap;
use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aicortex::Aicortex;
use aicortex::embedding_preflight;
use aicortex::embedding_service::{self, Gauges, METRICS_NAME, WORKER_NAME};
use aicortex_embed::static_model::EMBEDDINGS_TENSOR;
use aicortex_embed::{
    EmbeddingConfig, ModelId, ModelRegistry, ModelRevision, NoTransport, operator, stage_embedding,
};
use aicortex_types::{
    Actor, ActorId, Importance, Memory, MemoryBody, MemoryId, MemoryKind, MemoryParts, Provenance,
    Scope, SourceRef, SourceSystem, TrustClass,
};
use rahi_cli::{AppOutcome, Cell};
use rahi_kernel::Manifest;
use rahi_store::{
    EncKey, EncKeys, FailureDetail, RetryPolicy, Store, StoreConfig, StoreHandle, StoreSecrets,
    TxnBuilder, Value, Work,
};
use rahi_types::{Sub, UnixSeconds};
use ring::digest::{SHA256, digest};
use safetensors::Dtype;
use safetensors::tensor::TensorView;
use serde::Deserialize;
use tokio::sync::watch;

const SHIPPED_MANIFEST: &str = include_str!("../manifest.toml");

const TOKENIZER: &str = r#"{
  "normalizer": {"type": "BertNormalizer", "lowercase": true},
  "pre_tokenizer": {"type": "BertPreTokenizer"},
  "model": {"type": "WordPiece", "unk_token": "[UNK]",
            "vocab": {"[UNK]": 0, "red": 1, "green": 2}}
}"#;

const SCOPE: &str = "scope-a";

fn free_addr() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("free loopback port");
    listener.local_addr().expect("bound address")
}

struct Fixture {
    store: Store,
    directory: tempfile::TempDir,
}

impl Fixture {
    /// A single-voter node with the cell's migrations and the chassis sets.
    async fn migrated() -> Self {
        let directory = tempfile::tempdir().expect("temporary store directory");
        let config = StoreConfig {
            node_id: 1,
            nodes: Vec::new(),
            data_dir: directory.path().join("hiqlite"),
            raft_addr: free_addr(),
            api_addr: free_addr(),
            secrets: StoreSecrets {
                secret_raft: "raft-secret-for-app-tests".to_owned(),
                secret_api: "api-secret-for-app-tests".to_owned(),
                enc_keys: EncKeys {
                    active: "test".to_owned(),
                    keys: vec![EncKey {
                        id: "test".to_owned(),
                        key: vec![23_u8; 32],
                    }],
                },
            },
            backup_keep_days: 1,
            s3: None,
        };
        let store = Store::open(&config).await.expect("single-voter store");
        let handle = store.handle();
        handle
            .migrate(Aicortex::migrations())
            .await
            .expect("the cell's migrations apply");
        handle
            .migrate_sets(&[], &Aicortex::migration_sets())
            .await
            .expect("the chassis sets apply");
        Self { store, directory }
    }

    fn handle(&self) -> StoreHandle {
        self.store.handle()
    }

    fn models_dir(&self) -> PathBuf {
        self.directory.path().join("models")
    }

    async fn shutdown(self) {
        self.store.shutdown().await.expect("the node stops");
    }
}

fn sha(bytes: &[u8]) -> String {
    digest(&SHA256, bytes)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn weights() -> Vec<u8> {
    let data: Vec<u8> = [9.0_f32, 9.0, 1.0, 0.0, 0.0, 1.0]
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect();
    let view = TensorView::new(Dtype::F32, vec![3, 2], &data).expect("view");
    safetensors::serialize([(EMBEDDINGS_TENSOR, view)], None).expect("serialize")
}

/// A local configuration whose artifacts are present and match their pins.
fn local_env(models: &Path) -> BTreeMap<String, String> {
    std::fs::create_dir_all(models).expect("models directory");
    std::fs::write(models.join("model.safetensors"), weights()).expect("weights");
    std::fs::write(models.join("tokenizer.json"), TOKENIZER).expect("tokenizer");
    [
        ("AICORTEX_EMBED_MODEL", "static-fixture"),
        ("AICORTEX_EMBED_DIMS", "2"),
        ("AICORTEX_EMBED_MODELS_DIR", models.to_str().expect("utf-8")),
        ("AICORTEX_EMBED_WEIGHTS_FILE", "model.safetensors"),
        (
            "AICORTEX_EMBED_WEIGHTS_URL",
            "https://models.example/m.safetensors",
        ),
        ("AICORTEX_EMBED_WEIGHTS_SHA256", &sha(&weights())),
        ("AICORTEX_EMBED_TOKENIZER_FILE", "tokenizer.json"),
        (
            "AICORTEX_EMBED_TOKENIZER_URL",
            "https://models.example/tokenizer.json",
        ),
        (
            "AICORTEX_EMBED_TOKENIZER_SHA256",
            &sha(TOKENIZER.as_bytes()),
        ),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_owned(), value.to_owned()))
    .collect()
}

fn remote_env(endpoint: &str) -> BTreeMap<String, String> {
    [
        ("AICORTEX_EMBED_PROVIDER", "remote"),
        ("AICORTEX_EMBED_MODEL", "remote-model"),
        ("AICORTEX_EMBED_DIMS", "8"),
        ("AICORTEX_EMBED_ENDPOINT", endpoint),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_owned(), value.to_owned()))
    .collect()
}

fn no_env() -> BTreeMap<String, String> {
    BTreeMap::new()
}

fn shipped_manifest() -> Manifest {
    Manifest::parse(SHIPPED_MANIFEST).expect("the shipped manifest parses")
}

fn granting_manifest(host: &str) -> Manifest {
    Manifest::parse(&format!(
        r#"
schema_version = "1.0.0"

[app]
name = "aicortex"
org = "statecrafting"

[resources]
egress = ["{host}"]

[[capabilities]]
id = "embedding-egress"
kind = "http.egress"
resource = "{host}"

[services.embedding]
capabilities = ["embedding-egress"]

[ledger]
schema_version = "1.0.0"
max_record_bytes = 65536

[observability]
metrics_path = "/metrics"
otel = false

[auth]
operator_role = "aicortex_operator"

[contract]
version = "0.1.0"
"#
    ))
    .expect("manifest parses")
}

fn test_model() -> ModelRevision {
    ModelRevision {
        model_id: ModelId::new("test-model").expect("model id"),
        revision: 1,
        dims: 2,
        normalized: true,
        first_seen: UnixSeconds::new(1),
        active: true,
    }
}

fn memory(text: &str) -> Memory {
    let at = UnixSeconds::new(2);
    Memory::new(MemoryParts {
        id: MemoryId::now_v7(),
        scope: Scope::personal(Sub::new("alice")),
        kind: MemoryKind::Observation,
        body: MemoryBody::text(text),
        actor: Actor::human(ActorId::new("tester").expect("actor id")),
        provenance: Provenance::captured(
            SourceRef::new(SourceSystem::new("test").expect("source system")),
            at,
            at,
        ),
        trust: TrustClass::Assertion,
        importance: Importance::at(at).expect("importance"),
        created: at,
    })
}

async fn insert_scope(store: &StoreHandle) {
    store
        .execute(
            "INSERT INTO scope (scope_id, owner, kind, key, created) VALUES (?1, 'alice', 'personal', '', 1)",
            vec![Value::from(SCOPE)],
        )
        .await
        .expect("scope inserts");
}

/// A live memory row in the real schema, as capture would leave it.
async fn insert_memory(store: &StoreHandle, memory: &Memory) {
    let record = serde_json::to_string(memory).expect("memory serializes");
    store
        .execute(
            "INSERT INTO memory (id, scope_id, status, kind, trust, fingerprint, schema_version,
                                 body_bytes, created, updated, record)
             VALUES (?1, ?2, ?3, 'observation', 'assertion', ?1, 1, 10, 2, 2, ?4)",
            vec![
                Value::from(memory.id.to_string()),
                Value::from(SCOPE),
                Value::from(memory.status.label()),
                Value::from(record),
            ],
        )
        .await
        .expect("memory inserts");
    // Capture maintains the scope counter in the same transaction (012 B-6);
    // the live total the report reads comes from it.
    store
        .execute(
            "INSERT INTO scope_counter (scope_id, kind, status, count)
             VALUES (?1, 'observation', ?2, 1)
             ON CONFLICT (scope_id, kind, status) DO UPDATE SET count = count + 1",
            vec![Value::from(SCOPE), Value::from(memory.status.label())],
        )
        .await
        .expect("counter increments");
}

async fn activate(store: &StoreHandle, model: &ModelRevision) {
    let mut txn = TxnBuilder::new();
    ModelRegistry::activate(&mut txn, model).expect("active model accepted");
    store
        .txn(txn.into_statements())
        .await
        .expect("model activates");
}

async fn stage(store: &StoreHandle, id: MemoryId, model: &ModelRevision) {
    let mut txn = TxnBuilder::new();
    stage_embedding(&mut txn, SCOPE, id, model, UnixSeconds::new(2)).expect("work stages");
    store
        .txn(txn.into_statements())
        .await
        .expect("work commits");
}

/// Claim one staged job and exhaust its single attempt: a dead letter.
async fn dead_letter_one(store: &StoreHandle, model: &ModelRevision) {
    let processor = aicortex_embed::embedding_processor(model.revision);
    let claims = Work::next(
        store,
        aicortex_embed::EMBEDDING_NAMESPACE,
        &processor,
        "test-holder",
        Duration::from_secs(60),
        UnixSeconds::new(3),
        1,
    )
    .await
    .expect("work claims");
    let claim = claims.first().expect("one staged job");
    let mut txn = TxnBuilder::new();
    Work::fail(
        &mut txn,
        claim,
        &FailureDetail {
            class: "provider_error".to_owned(),
            detail: None,
        },
        &RetryPolicy {
            max_attempts: 1,
            base: Duration::from_secs(1),
            cap: Duration::from_secs(1),
        },
        UnixSeconds::new(3),
    )
    .expect("failure stages");
    store
        .txn(txn.into_statements())
        .await
        .expect("failure commits");
}

async fn verdict(
    env: &BTreeMap<String, String>,
    manifest: &Manifest,
    store: &StoreHandle,
) -> rahi_cli::AppVerdict {
    embedding_preflight::verdict(
        env,
        manifest,
        store,
        UnixSeconds::new(10),
        Duration::from_secs(5),
    )
    .await
}

#[derive(Deserialize)]
struct CountRow {
    count: i64,
}

async fn count(store: &StoreHandle, sql: &'static str) -> i64 {
    let rows: Vec<CountRow> = store.query_consistent(sql, vec![]).await.expect("count");
    rows.first().map_or(0, |row| row.count)
}

/// A stop signal the test controls.
fn stop_signal() -> (
    watch::Sender<bool>,
    impl Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
    + Clone
    + Send
    + Sync
    + 'static,
) {
    let (sender, receiver) = watch::channel(false);
    let stop = move || {
        let mut receiver = receiver.clone();
        Box::pin(async move {
            let _ = receiver.wait_for(|stopped| *stopped).await;
        }) as std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
    };
    (sender, stop)
}

async fn eventually<F, Fut>(what: &str, mut condition: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = Instant::now() + Duration::from_secs(40);
    while !condition().await {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

#[test]
fn the_cell_declares_exactly_the_embedding_check() {
    let checks = Aicortex::preflight_checks();
    let names: Vec<&str> = checks.iter().map(rahi_cli::AppCheck::name).collect();
    assert_eq!(names, vec!["embedding"]);
}

/// FR-005 through the check: a remote provider whose host the ceiling does
/// not admit fails with the named capability error, and adding the host to
/// the ceiling clears that failure.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr005_a_remote_host_absent_from_the_ceiling_fails_the_check_by_name() {
    let fixture = Fixture::migrated().await;
    let store = fixture.handle();
    let env = remote_env("https://models.example/v1/embed");

    let denied = verdict(&env, &shipped_manifest(), &store).await;
    assert_eq!(denied.outcome, AppOutcome::Fail, "{denied:?}");
    assert!(
        denied.detail.contains("embedding.remote.egress"),
        "{denied:?}"
    );
    assert!(denied.detail.contains("models.example"), "{denied:?}");

    let other = verdict(&env, &granting_manifest("other.example"), &store).await;
    assert_eq!(other.outcome, AppOutcome::Fail, "{other:?}");

    let admitted = verdict(&env, &granting_manifest("models.example"), &store).await;
    assert_ne!(admitted.outcome, AppOutcome::Fail, "{admitted:?}");
    fixture.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_malformed_configuration_fails_the_check_naming_the_variable() {
    let fixture = Fixture::migrated().await;
    let mut env = remote_env("https://models.example/v1/embed");
    env.remove("AICORTEX_EMBED_MODEL");
    let failed = verdict(&env, &shipped_manifest(), &fixture.handle()).await;
    assert_eq!(failed.outcome, AppOutcome::Fail);
    assert!(failed.detail.contains("AICORTEX_EMBED_MODEL"), "{failed:?}");
    fixture.shutdown().await;
}

/// AC-2 on a fresh deployment: nothing configured, nothing active, nothing
/// queued, and the report says so rather than printing nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ac2_a_fresh_deployment_reports_no_model_and_an_empty_queue() {
    let fixture = Fixture::migrated().await;
    let result = verdict(&no_env(), &shipped_manifest(), &fixture.handle()).await;
    assert_eq!(result.outcome, AppOutcome::Pass, "{result:?}");
    for line in [
        "provider: off (captures stage no embedding work)",
        "active model: none",
        "queue: pending=0 dead=0 quarantined=0 oldest_pending_seconds=none",
        "coverage: no model revision recorded",
        "scopes read: 0",
    ] {
        assert!(
            result.report.iter().any(|l| l == line),
            "{line}: {result:?}"
        );
    }
    fixture.shutdown().await;
}

/// AC-2 with a model and queued work, and FR-003 through the check: a dead
/// letter turns the verdict into a warning that counts it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ac2_fr003_the_report_names_the_model_the_queue_and_warns_on_a_dead_letter() {
    let fixture = Fixture::migrated().await;
    let store = fixture.handle();
    insert_scope(&store).await;
    let model = test_model();
    activate(&store, &model).await;
    let live = memory("Alice keeps a live memory.");
    insert_memory(&store, &live).await;
    stage(&store, MemoryId::now_v7(), &model).await;

    let pending = verdict(&no_env(), &shipped_manifest(), &store).await;
    assert_eq!(pending.outcome, AppOutcome::Warn, "{pending:?}");
    for line in [
        "active model: test-model@1 dims=2 normalized=true",
        "queue: pending=1 dead=0 quarantined=0 oldest_pending_seconds=8",
        "scopes read: 1",
    ] {
        assert!(
            pending.report.iter().any(|l| l == line),
            "{line}: {pending:?}"
        );
    }
    // The warning here is incomplete coverage, not a dead letter.
    assert!(
        !pending.detail.contains("dead embedding work item"),
        "{pending:?}"
    );

    dead_letter_one(&store, &model).await;
    let dead = verdict(&no_env(), &shipped_manifest(), &store).await;
    assert_eq!(dead.outcome, AppOutcome::Warn, "{dead:?}");
    assert!(
        dead.detail.contains("1 dead embedding work item(s)"),
        "{dead:?}"
    );
    assert!(
        dead.report
            .iter()
            .any(|l| l.starts_with("queue: pending=0 dead=1 ")),
        "{dead:?}"
    );
    assert!(
        dead.report.iter().any(|l| l == "coverage revision 1: 0/1"),
        "{dead:?}"
    );
    fixture.shutdown().await;
}

/// A time budget that has already elapsed stops the walk and says so.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_exhausted_budget_is_reported_as_a_partial_figure() {
    let fixture = Fixture::migrated().await;
    let store = fixture.handle();
    insert_scope(&store).await;
    let partial = embedding_preflight::verdict(
        &no_env(),
        &shipped_manifest(),
        &store,
        UnixSeconds::new(10),
        Duration::ZERO,
    )
    .await;
    assert_eq!(partial.outcome, AppOutcome::Warn, "{partial:?}");
    assert!(
        partial
            .report
            .iter()
            .any(|l| l.contains("time budget reached")),
        "{partial:?}"
    );
    fixture.shutdown().await;
}

/// Local artifacts that are absent are reported, not hidden: no fetch
/// transport is linked, so an operator must supply them.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn absent_local_artifacts_are_a_warning() {
    let fixture = Fixture::migrated().await;
    let models = fixture.models_dir();
    let env = local_env(&models);
    std::fs::remove_file(models.join("model.safetensors")).expect("remove weights");
    // The weights host is absent from the shipped ceiling, so the check
    // refuses first (B-5): the fetch would need egress.
    let refused = verdict(&env, &shipped_manifest(), &fixture.handle()).await;
    assert_eq!(refused.outcome, AppOutcome::Fail, "{refused:?}");
    assert!(
        refused.detail.contains("embedding.weights.egress"),
        "{refused:?}"
    );
    let warned = verdict(
        &env,
        &granting_manifest("models.example"),
        &fixture.handle(),
    )
    .await;
    assert_eq!(warned.outcome, AppOutcome::Warn, "{warned:?}");
    assert!(
        warned
            .report
            .iter()
            .any(|l| l.starts_with("artifact absent:")),
        "{warned:?}"
    );
    fixture.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn services_are_the_collector_alone_when_no_provider_is_configured() {
    let fixture = Fixture::migrated().await;
    let (_sender, stop) = stop_signal();
    let services = embedding_service::build(
        EmbeddingConfig::Disabled,
        fixture.handle(),
        Gauges::new().expect("gauges"),
        None,
        stop,
    );
    let names: Vec<&str> = services
        .iter()
        .map(rahi_cli::ManagedService::name)
        .collect();
    assert_eq!(names, vec![METRICS_NAME]);
    fixture.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn services_include_the_named_worker_when_a_provider_is_configured() {
    let fixture = Fixture::migrated().await;
    let env = local_env(&fixture.models_dir());
    let config = EmbeddingConfig::from_env(&env).expect("configuration parses");
    let (_sender, stop) = stop_signal();
    let services = embedding_service::build(
        config,
        fixture.handle(),
        Gauges::new().expect("gauges"),
        None,
        stop,
    );
    let names: Vec<&str> = services
        .iter()
        .map(rahi_cli::ManagedService::name)
        .collect();
    assert_eq!(names, vec![METRICS_NAME, WORKER_NAME]);
    fixture.shutdown().await;
}

/// B-2 end to end: a worker started before any model is active idles, binds
/// when an operator activates the configured model, embeds a staged memory,
/// and returns `Ok` only when told to stop.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn b2_the_worker_binds_after_activation_embeds_and_stops_on_shutdown() {
    let fixture = Fixture::migrated().await;
    let store = fixture.handle();
    let env = local_env(&fixture.models_dir());
    let config = EmbeddingConfig::from_env(&env).expect("configuration parses");
    let (sender, stop) = stop_signal();

    let running = tokio::spawn(embedding_service::work(
        config.clone(),
        store.clone(),
        None,
        stop,
    ));
    // With no model active the service idles: it neither returns nor fails.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        !running.is_finished(),
        "the worker returned before shutdown"
    );

    insert_scope(&store).await;
    let live = memory("red green red");
    insert_memory(&store, &live).await;
    let activation = operator::activate_configured(
        &config,
        &shipped_manifest(),
        None,
        &Sub::new("test-operator"),
        NoTransport,
        &store,
        UnixSeconds::new(5),
    )
    .await
    .expect("the configured model activates");
    assert_eq!(activation.model.revision, 1);
    let model = ModelRegistry::active(&store)
        .await
        .expect("registry reads")
        .expect("a model is active");
    stage(&store, live.id, &model).await;

    eventually("the staged memory to be embedded", || {
        let store = store.clone();
        async move { count(&store, "SELECT COUNT(*) AS count FROM embedding").await == 1 }
    })
    .await;
    assert!(
        !running.is_finished(),
        "the worker returned before shutdown"
    );

    sender.send_replace(true);
    let outcome = tokio::time::timeout(Duration::from_secs(20), running)
        .await
        .expect("the worker stops on shutdown")
        .expect("the worker does not panic");
    assert!(outcome.is_ok(), "{outcome:?}");
    fixture.shutdown().await;
}

/// B-3: the gauges follow the queue and the collector returns `Ok` on stop.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b3_the_collector_publishes_the_dead_letter_count_and_stops_on_shutdown() {
    let fixture = Fixture::migrated().await;
    let store = fixture.handle();
    insert_scope(&store).await;
    let model = test_model();
    activate(&store, &model).await;
    stage(&store, MemoryId::now_v7(), &model).await;
    dead_letter_one(&store, &model).await;

    let gauges = Gauges::new().expect("gauges");
    let registry = prometheus::Registry::new();
    gauges.register(&registry).expect("gauges register once");
    assert!(gauges.register(&registry).is_err(), "a name registers once");

    let (sender, stop) = stop_signal();
    let running = tokio::spawn(embedding_service::collect(
        store.clone(),
        gauges.clone(),
        stop,
    ));
    eventually("the dead-letter gauge", || {
        let gauges = gauges.clone();
        async move { gauges.dead() == 1 }
    })
    .await;
    assert_eq!(gauges.pending(), 0);
    let exposition: Vec<String> = registry
        .gather()
        .iter()
        .map(|family| family.name().to_owned())
        .collect();
    assert!(
        exposition.contains(&embedding_service::DEAD_LETTERS.to_owned())
            && exposition.contains(&embedding_service::OLDEST_PENDING_AGE.to_owned()),
        "{exposition:?}"
    );

    sender.send_replace(true);
    let outcome = tokio::time::timeout(Duration::from_secs(20), running)
        .await
        .expect("the collector stops on shutdown")
        .expect("the collector does not panic");
    assert!(outcome.is_ok(), "{outcome:?}");
    fixture.shutdown().await;
}
