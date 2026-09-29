#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used
)]

use std::net::{SocketAddr, TcpListener};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use aicortex_embed::local::{LocalEngine, LocalProvider, WeightArtifact};
use aicortex_embed::migrations::{EMBEDDING_MIGRATION_VERSION, migration};
use aicortex_embed::provider::{EmbeddingProvider, ModelId, Vector};
use aicortex_embed::registry::{ModelRegistry, ModelRevision};
use aicortex_embed::{
    ActiveEmbedding, ChunkConfig, Chunker, EmbeddingPreflight, EmbeddingWorker, WorkerConfig,
    embedding_processor, embedding_work_key, queue_health, stage_active_embedding, stage_embedding,
    stage_live_embedding, stage_reembedding_batch,
};
use aicortex_types::{
    Actor, ActorId, Importance, Memory, MemoryBody, MemoryId, MemoryKind, MemoryParts, Provenance,
    Scope, SourceRef, SourceSystem, TrustClass,
};
use rahi_store::{
    EncKey, EncKeys, EraseScope, ProcessingKey, ReceiptKey, Receipts, RetryPolicy, Statement,
    Store, StoreConfig, StoreHandle, StoreSecrets, TxnBuilder, Value, Work, coordination_set,
    receipt_set,
};
use rahi_types::{Error, Sub, UnixSeconds};
use ring::digest::{SHA256, digest};
use serde::Deserialize;

#[derive(Clone, Copy, Debug)]
struct FixedEngine;

impl LocalEngine for FixedEngine {
    async fn infer(&self, _weights: &[u8], batch: &[&str]) -> Result<Vec<Vector>, Error> {
        (0..batch.len())
            .map(|_| Vector::new(vec![1.0, 0.0]))
            .collect()
    }
}

#[derive(Clone, Copy, Debug)]
struct TestProvider {
    fail: bool,
}

#[derive(Clone, Copy, Debug)]
struct SlowProvider {
    delay: Duration,
}

#[derive(Clone, Copy, Debug)]
struct OtherProvider;

#[derive(Clone, Debug)]
struct OversizedProvider {
    called: Arc<AtomicBool>,
}

#[derive(Clone, Copy, Debug)]
struct LayoutProvider {
    dims: u16,
    normalized: bool,
}

#[derive(Clone, Debug)]
struct ErasingProvider {
    store: StoreHandle,
    scope_id: String,
    memory_ids: Vec<MemoryId>,
    erased: Arc<AtomicBool>,
}

#[derive(Clone, Debug)]
struct QuarantiningProvider {
    store: StoreHandle,
    memory_id: MemoryId,
}

#[derive(Clone, Debug)]
struct SwitchingProvider {
    store: StoreHandle,
    replacement: ModelRevision,
}

#[derive(Clone, Debug)]
struct DeactivatingProvider {
    store: StoreHandle,
    deactivated: Arc<AtomicBool>,
}

impl EmbeddingProvider for ErasingProvider {
    fn id(&self) -> ModelId {
        ModelId::new("test-model").expect("static model id")
    }

    fn dims(&self) -> u16 {
        2
    }

    fn normalized(&self) -> bool {
        true
    }

    async fn embed(&self, batch: &[&str]) -> Result<Vec<Vector>, Error> {
        if !self.erased.swap(true, Ordering::SeqCst) {
            let mut txn = TxnBuilder::new();
            for memory_id in &self.memory_ids {
                txn.push(Statement::with_params(
                    "UPDATE memory SET status = 'erased' WHERE scope_id = ?1 AND id = ?2",
                    vec![
                        Value::from(self.scope_id.as_str()),
                        Value::from(memory_id.to_string()),
                    ],
                ));
                let key = ReceiptKey::new(
                    self.scope_id.as_str(),
                    aicortex_store::EMBEDDING_NAMESPACE,
                    memory_id.to_string(),
                )?;
                Receipts::stage_erasure(&mut txn, &EraseScope::Identity(key), UnixSeconds::new(3));
            }
            self.store.txn(txn.into_statements()).await?;
        }
        batch.iter().map(|_| Vector::new(vec![1.0, 0.0])).collect()
    }
}

impl EmbeddingProvider for QuarantiningProvider {
    fn id(&self) -> ModelId {
        ModelId::new("test-model").expect("static model id")
    }

    fn dims(&self) -> u16 {
        2
    }

    fn normalized(&self) -> bool {
        true
    }

    async fn embed(&self, batch: &[&str]) -> Result<Vec<Vector>, Error> {
        self.store
            .execute(
                "UPDATE memory SET status = 'quarantined' WHERE scope_id = ?1 AND id = ?2",
                vec![
                    Value::from("scope-a"),
                    Value::from(self.memory_id.to_string()),
                ],
            )
            .await?;
        batch.iter().map(|_| Vector::new(vec![1.0, 0.0])).collect()
    }
}

impl EmbeddingProvider for SwitchingProvider {
    fn id(&self) -> ModelId {
        ModelId::new("test-model").expect("static model id")
    }

    fn dims(&self) -> u16 {
        2
    }

    fn normalized(&self) -> bool {
        true
    }

    async fn embed(&self, batch: &[&str]) -> Result<Vec<Vector>, Error> {
        let mut txn = TxnBuilder::new();
        ModelRegistry::activate(&mut txn, &self.replacement)?;
        self.store.txn(txn.into_statements()).await?;
        batch.iter().map(|_| Vector::new(vec![1.0, 0.0])).collect()
    }
}

impl EmbeddingProvider for DeactivatingProvider {
    fn id(&self) -> ModelId {
        ModelId::new("test-model").expect("static model id")
    }

    fn dims(&self) -> u16 {
        2
    }

    fn normalized(&self) -> bool {
        true
    }

    async fn embed(&self, batch: &[&str]) -> Result<Vec<Vector>, Error> {
        if !self.deactivated.swap(true, Ordering::SeqCst) {
            self.store
                .execute("UPDATE embedding_model SET active = 0", vec![])
                .await?;
        }
        batch.iter().map(|_| Vector::new(vec![1.0, 0.0])).collect()
    }
}

impl EmbeddingProvider for TestProvider {
    fn id(&self) -> ModelId {
        ModelId::new("test-model").expect("static model id")
    }

    fn dims(&self) -> u16 {
        2
    }

    fn normalized(&self) -> bool {
        true
    }

    async fn embed(&self, batch: &[&str]) -> Result<Vec<Vector>, Error> {
        if self.fail {
            return Err(Error::Upstream("injected provider failure".to_owned()));
        }
        batch.iter().map(|_| Vector::new(vec![1.0, 0.0])).collect()
    }
}

impl EmbeddingProvider for SlowProvider {
    fn id(&self) -> ModelId {
        ModelId::new("test-model").expect("static model id")
    }

    fn dims(&self) -> u16 {
        2
    }

    fn normalized(&self) -> bool {
        true
    }

    async fn embed(&self, batch: &[&str]) -> Result<Vec<Vector>, Error> {
        tokio::time::sleep(self.delay).await;
        batch.iter().map(|_| Vector::new(vec![1.0, 0.0])).collect()
    }
}

impl EmbeddingProvider for OtherProvider {
    fn id(&self) -> ModelId {
        ModelId::new("other-model").expect("static model id")
    }

    fn dims(&self) -> u16 {
        2
    }

    fn normalized(&self) -> bool {
        true
    }

    async fn embed(&self, batch: &[&str]) -> Result<Vec<Vector>, Error> {
        batch.iter().map(|_| Vector::new(vec![1.0, 0.0])).collect()
    }
}

impl EmbeddingProvider for OversizedProvider {
    fn id(&self) -> ModelId {
        ModelId::new("oversized-model").expect("static model id")
    }

    fn dims(&self) -> u16 {
        u16::MAX
    }

    fn normalized(&self) -> bool {
        false
    }

    async fn embed(&self, batch: &[&str]) -> Result<Vec<Vector>, Error> {
        self.called.store(true, Ordering::SeqCst);
        batch
            .iter()
            .map(|_| {
                let mut values = vec![0.0; usize::from(u16::MAX)];
                values[0] = 1.0;
                Vector::new(values)
            })
            .collect()
    }
}

impl EmbeddingProvider for LayoutProvider {
    fn id(&self) -> ModelId {
        ModelId::new("test-model").expect("static model id")
    }

    fn dims(&self) -> u16 {
        self.dims
    }

    fn normalized(&self) -> bool {
        self.normalized
    }

    async fn embed(&self, batch: &[&str]) -> Result<Vec<Vector>, Error> {
        batch
            .iter()
            .map(|_| Vector::new(vec![0.0; usize::from(self.dims)]))
            .collect()
    }
}

struct Fixture {
    store: Store,
    _directory: tempfile::TempDir,
}

impl Fixture {
    async fn migrated() -> Self {
        let directory = tempfile::tempdir().expect("temporary store directory");
        let data_dir = directory.path().join("hiqlite");
        let mut last = None;
        let store = loop {
            match Store::open(&store_config(data_dir.clone())).await {
                Ok(store) => break store,
                Err(error)
                    if last.as_ref().map_or(0, |attempt: &u8| *attempt) < 4
                        && error.message().contains("Address already in use") =>
                {
                    last = Some(last.map_or(1, |attempt| attempt + 1));
                }
                Err(error) => panic!("single-voter store opens: {error}"),
            }
        };
        let handle = store.handle();
        handle
            .migrate_sets(&[], &[coordination_set(), receipt_set()])
            .await
            .expect("Rahi work migrations apply");
        handle
            .execute(
                "CREATE TABLE memory (
                    scope_id TEXT NOT NULL,
                    id TEXT NOT NULL,
                    status TEXT NOT NULL,
                    record TEXT NOT NULL,
                    PRIMARY KEY (scope_id, id)
                )",
                vec![],
            )
            .await
            .expect("memory fixture table applies");
        for statement in [
            "CREATE TABLE scope_counter (
                scope_id TEXT NOT NULL,
                kind TEXT NOT NULL,
                status TEXT NOT NULL,
                count INTEGER NOT NULL,
                PRIMARY KEY (scope_id, kind, status)
            )",
            "CREATE TRIGGER memory_counter_insert AFTER INSERT ON memory BEGIN
                INSERT INTO scope_counter (scope_id, kind, status, count)
                VALUES (NEW.scope_id, 'observation', NEW.status, 1)
                ON CONFLICT (scope_id, kind, status)
                DO UPDATE SET count = scope_counter.count + 1;
            END",
            "CREATE TRIGGER memory_counter_update AFTER UPDATE OF status ON memory
             WHEN OLD.status <> NEW.status BEGIN
                UPDATE scope_counter SET count = max(0, count - 1)
                WHERE scope_id = OLD.scope_id AND kind = 'observation'
                  AND status = OLD.status;
                INSERT INTO scope_counter (scope_id, kind, status, count)
                VALUES (NEW.scope_id, 'observation', NEW.status, 1)
                ON CONFLICT (scope_id, kind, status)
                DO UPDATE SET count = scope_counter.count + 1;
            END",
            "CREATE TRIGGER memory_counter_delete AFTER DELETE ON memory BEGIN
                UPDATE scope_counter SET count = max(0, count - 1)
                WHERE scope_id = OLD.scope_id AND kind = 'observation'
                  AND status = OLD.status;
            END",
        ] {
            handle
                .execute(statement, vec![])
                .await
                .expect("memory counter fixture applies");
        }
        for statement in migration().sql.split(';').map(str::trim) {
            if !statement.is_empty() {
                handle
                    .execute(statement.to_owned(), vec![])
                    .await
                    .expect("embedding migration statement applies");
            }
        }
        Self {
            store,
            _directory: directory,
        }
    }

    fn handle(&self) -> StoreHandle {
        self.store.handle()
    }

    async fn shutdown(self) {
        self.store.shutdown().await.expect("test store stops");
    }
}

fn free_addr() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("free loopback port");
    listener.local_addr().expect("bound address")
}

fn store_config(data_dir: PathBuf) -> StoreConfig {
    StoreConfig {
        node_id: 1,
        nodes: Vec::new(),
        data_dir,
        raft_addr: free_addr(),
        api_addr: free_addr(),
        secrets: StoreSecrets {
            secret_raft: "raft-secret-for-embed-tests".to_owned(),
            secret_api: "api-secret-for-embed-tests".to_owned(),
            enc_keys: EncKeys {
                active: "test".to_owned(),
                keys: vec![EncKey {
                    id: "test".to_owned(),
                    key: vec![19_u8; 32],
                }],
            },
        },
        backup_keep_days: 1,
        s3: None,
    }
}

fn test_model(active: bool) -> ModelRevision {
    ModelRevision {
        model_id: ModelId::new("test-model").expect("model id"),
        revision: 1,
        dims: 2,
        normalized: true,
        first_seen: UnixSeconds::new(1),
        active,
    }
}

fn test_memory(owner: &str, text: &str, at: u64) -> Memory {
    let at = UnixSeconds::new(at);
    Memory::new(MemoryParts {
        id: MemoryId::now_v7(),
        scope: Scope::personal(Sub::new(owner)),
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

async fn insert_memory(store: &StoreHandle, scope_id: &str, memory: &Memory) {
    store
        .execute(
            "INSERT INTO memory (scope_id, id, status, record) VALUES (?1, ?2, ?3, ?4)",
            vec![
                Value::from(scope_id),
                Value::from(memory.id.to_string()),
                Value::from(memory.status.label()),
                Value::from(serde_json::to_string(memory).expect("memory serializes")),
            ],
        )
        .await
        .expect("memory inserts");
}

async fn activate(store: &StoreHandle, model: &ModelRevision) {
    let mut txn = TxnBuilder::new();
    ModelRegistry::activate(&mut txn, model).expect("active model accepted");
    store
        .txn(txn.into_statements())
        .await
        .expect("model activates");
}

fn worker(
    provider: TestProvider,
    holder: &str,
    max_attempts: u32,
) -> EmbeddingWorker<TestProvider> {
    worker_for_model(provider, test_model(true), holder, max_attempts)
}

fn worker_for_model(
    provider: TestProvider,
    model: ModelRevision,
    holder: &str,
    max_attempts: u32,
) -> EmbeddingWorker<TestProvider> {
    EmbeddingWorker::new(
        provider,
        model,
        Chunker::new(ChunkConfig {
            threshold_bytes: 256,
            target_bytes: 128,
            overlap_bytes: 16,
            max_chunks_per_memory: 512,
        })
        .expect("chunk config"),
        WorkerConfig {
            batch_size: 16,
            hold_for: Duration::from_secs(5),
            retry: RetryPolicy {
                max_attempts,
                base: Duration::from_secs(1),
                cap: Duration::from_secs(2),
            },
        },
        holder,
    )
    .expect("worker config")
}

#[derive(Deserialize)]
struct CountRow {
    count: i64,
}

#[derive(Deserialize)]
struct ChunkIdRow {
    chunk_id: String,
}

async fn count(store: &StoreHandle, sql: &str, params: Vec<Value>) -> i64 {
    let rows: Vec<CountRow> = store
        .query_consistent(sql.to_owned(), params)
        .await
        .expect("count query succeeds");
    rows.first().map_or(0, |row| row.count)
}

#[test]
fn vector_bytes_are_little_endian_and_dimension_checked() {
    let vector = Vector::new(vec![1.0, -2.5]).expect("finite vector");
    let bytes = vector.to_le_bytes();
    assert_eq!(&bytes[..4], &1.0_f32.to_le_bytes());
    assert_eq!(&bytes[4..], &(-2.5_f32).to_le_bytes());
    assert_eq!(
        Vector::from_le_bytes(&bytes, 2).expect("valid stored vector"),
        vector
    );
    assert!(Vector::from_le_bytes(&bytes, 3).is_err());
}

#[test]
fn embedding_sources_do_not_own_memory_table_sql() {
    for (name, source) in [
        ("worker.rs", include_str!("../src/worker.rs")),
        ("registry.rs", include_str!("../src/registry.rs")),
    ] {
        let source = source.to_ascii_lowercase();
        for fragment in ["from memory", "join memory"] {
            assert!(
                !source.contains(fragment),
                "{name} bypasses the storage repository with {fragment:?}"
            );
        }
    }
    let store_source = include_str!("../../aicortex-store/src/embedding_memory.rs");
    assert!(store_source.contains("memory.scope_id = ?1"));
    assert!(store_source.contains("processing.tenant = ?1"));
}

#[test]
fn migration_has_exact_tables_and_follows_existing_versions() {
    let migration = migration();
    assert_eq!(EMBEDDING_MIGRATION_VERSION, 9);
    assert_eq!(migration.version, 9);
    for table in ["embedding_model", "chunk", "embedding"] {
        assert!(migration.sql.contains(&format!("CREATE TABLE {table}")));
        assert!(
            !migration
                .sql
                .contains(&format!("CREATE TABLE IF NOT EXISTS {table}"))
        );
    }
    assert!(migration.sql.contains("scope_id TEXT NOT NULL"));
    assert!(migration.sql.contains("chunk_id TEXT NOT NULL UNIQUE"));
    assert!(migration.sql.contains("CHECK (length(vector) = dims * 4)"));
    assert!(
        migration
            .sql
            .contains("PRIMARY KEY (scope_id, memory_id, model_revision")
    );
    assert!(!migration.additive);
}

#[test]
fn staging_requires_an_active_revision_and_capture_uses_registry_selection() {
    let mut txn = TxnBuilder::new();
    let id = MemoryId::now_v7();
    assert!(
        stage_embedding(
            &mut txn,
            "scope-a",
            id,
            &test_model(false),
            UnixSeconds::new(1),
        )
        .is_err()
    );
    let active = ActiveEmbedding {
        model_id: "test-model".to_owned(),
        revision: 1,
    };
    stage_active_embedding(&mut txn, "scope-a", id, &active, UnixSeconds::new(1))
        .expect("capture work statement");
    let statements = format!("{:?}", txn.statements());
    assert!(statements.contains("INSERT INTO embedding_model"));
    assert!(statements.contains("ON CONFLICT"));
}

#[test]
fn registry_rejects_an_inactive_revision() {
    let mut txn = TxnBuilder::new();
    let error = ModelRegistry::activate(&mut txn, &test_model(false))
        .expect_err("inactive model is not an activation request");
    assert!(error.message().contains("marked inactive"));
    assert_eq!(txn.len(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn active_model_selection_is_required_before_staging() {
    let fixture = Fixture::migrated().await;
    let store = fixture.handle();
    assert!(aicortex_store::active_embedding(&store).await.is_err());
    assert_eq!(
        count(
            &store,
            "SELECT COUNT(*) AS count FROM rahi_processing",
            vec![],
        )
        .await,
        0
    );

    let mut txn = TxnBuilder::new();
    stage_embedding(
        &mut txn,
        "scope-a",
        MemoryId::now_v7(),
        &test_model(true),
        UnixSeconds::new(1),
    )
    .expect("the caller's claimed-active model has a valid shape");
    let error = store
        .txn(txn.into_statements())
        .await
        .expect_err("an unregistered revision aborts staging");
    assert!(error.message().contains("NOT NULL"));
    assert_eq!(
        count(
            &store,
            "SELECT COUNT(*) AS count FROM rahi_processing",
            vec![],
        )
        .await,
        0
    );
    fixture.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn activating_the_same_revision_preserves_its_first_seen_instant() {
    let fixture = Fixture::migrated().await;
    let store = fixture.handle();
    let original = test_model(true);
    activate(&store, &original).await;

    let restarted = ModelRevision {
        first_seen: UnixSeconds::new(99),
        ..original.clone()
    };
    activate(&store, &restarted).await;

    let active = ModelRegistry::active(&store)
        .await
        .expect("active model reads")
        .expect("model remains active");
    assert_eq!(active, original);
    fixture.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_inactive_newer_revision_prevents_reactivating_an_older_revision() {
    let fixture = Fixture::migrated().await;
    let store = fixture.handle();
    let original = test_model(true);
    activate(&store, &original).await;
    let replacement = ModelRevision {
        revision: 2,
        first_seen: UnixSeconds::new(2),
        ..original.clone()
    };
    activate(&store, &replacement).await;
    store
        .execute("UPDATE embedding_model SET active = 0", vec![])
        .await
        .expect("newer model deactivates");

    let mut txn = TxnBuilder::new();
    ModelRegistry::activate(&mut txn, &original).expect("active model accepted");
    let error = store
        .txn(txn.into_statements())
        .await
        .expect_err("an older revision cannot be reactivated");
    assert!(error.message().contains("NOT NULL"));
    assert!(
        ModelRegistry::active(&store)
            .await
            .expect("active model reads")
            .is_none()
    );
    fixture.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn claimed_work_stays_reportable_when_no_model_is_active() {
    let fixture = Fixture::migrated().await;
    let store = fixture.handle();
    let model = test_model(true);
    activate(&store, &model).await;
    let memory = test_memory("alice", "Inactive model work must remain visible.", 2);
    insert_memory(&store, "scope-a", &memory).await;
    let mut txn = TxnBuilder::new();
    stage_embedding(&mut txn, "scope-a", memory.id, &model, UnixSeconds::new(2))
        .expect("work stages");
    store
        .txn(txn.into_statements())
        .await
        .expect("work commits");
    store
        .execute("UPDATE embedding_model SET active = 0", vec![])
        .await
        .expect("model deactivates");

    let report = worker(TestProvider { fail: false }, "inactive-worker", 2)
        .drain(&store, UnixSeconds::new(3))
        .await
        .expect("inactive work remains reportable");
    assert_eq!(report.claimed, 0);
    assert_eq!(report.completed, 0);
    assert_eq!(report.failed, 0);
    assert_eq!(
        count(
            &store,
            "SELECT attempt AS count FROM rahi_processing LIMIT 1",
            vec![],
        )
        .await,
        0
    );
    let repeated = worker(TestProvider { fail: false }, "inactive-worker", 2)
        .drain(&store, UnixSeconds::new(10))
        .await
        .expect("repeated inactive drain preserves the retry budget");
    assert_eq!(repeated.claimed, 0);
    assert_eq!(
        count(
            &store,
            "SELECT attempt AS count FROM rahi_processing LIMIT 1",
            vec![],
        )
        .await,
        0
    );
    let health = queue_health(&store, "scope-a", UnixSeconds::new(3))
        .await
        .expect("queue health reads");
    assert_eq!(health.pending, 1);
    assert_eq!(health.dead, 0);

    activate(&store, &model).await;
    let first_failure = worker(TestProvider { fail: true }, "active-worker", 2)
        .drain(&store, UnixSeconds::new(11))
        .await
        .expect("the first provider failure still has retry budget");
    assert_eq!(first_failure.failed, 1);
    assert_eq!(first_failure.dead, 0);
    fixture.shutdown().await;
}

#[test]
fn staging_is_idempotent_per_memory_and_model_revision() {
    let id = MemoryId::now_v7();
    let model = ModelRevision {
        model_id: ModelId::new("configured-local-model").expect("model id"),
        revision: 2,
        dims: 2,
        normalized: true,
        first_seen: UnixSeconds::new(10),
        active: true,
    };
    let key = embedding_work_key("scope-digest", id, &model).expect("work key");
    assert_eq!(key.revision, 2);
    assert_eq!(key.processor, "embed.r2");
    assert_eq!(key.processor_revision, "configured-local-model");

    let mut txn = TxnBuilder::new();
    stage_embedding(&mut txn, "scope-digest", id, &model, UnixSeconds::new(11))
        .expect("first stage");
    stage_embedding(&mut txn, "scope-digest", id, &model, UnixSeconds::new(11))
        .expect("repeat stage");
    assert_eq!(txn.len(), 4);
    let statements = format!("{:?}", txn.statements());
    assert!(statements.contains("rahi_processing"));
    assert!(statements.contains("ON CONFLICT"));
}

#[test]
fn local_provider_boots_only_with_verified_weights() {
    let directory = tempfile::tempdir().expect("temporary models directory");
    let path = directory.path().join("model.bin");
    let bytes = b"fixed test weights";
    std::fs::write(&path, bytes).expect("write weights");
    let sha256 = digest(&SHA256, bytes)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let provider = LocalProvider::boot(
        ModelId::new("configured-local-model").expect("model id"),
        2,
        true,
        WeightArtifact {
            path,
            url: "https://models.example/model.bin".to_owned(),
            sha256,
        },
        directory.path(),
        FixedEngine,
    )
    .expect("verified provider");
    assert_eq!(provider.id().as_str(), "configured-local-model");
    assert_eq!(provider.dims(), 2);
    assert!(provider.normalized());
    let debug = format!("{provider:?}");
    assert!(debug.contains("weights_bytes: 18"), "{debug}");
    assert!(!debug.contains("fixed test weights"), "{debug}");
}

#[derive(Clone, Debug)]
struct CheckedWeightsEngine {
    expected: Vec<u8>,
}

impl LocalEngine for CheckedWeightsEngine {
    async fn infer(&self, weights: &[u8], batch: &[&str]) -> Result<Vec<Vector>, Error> {
        assert_eq!(weights, self.expected);
        (0..batch.len())
            .map(|_| Vector::new(vec![1.0, 0.0]))
            .collect()
    }
}

#[tokio::test]
async fn local_provider_keeps_the_verified_weight_snapshot() {
    let directory = tempfile::tempdir().expect("temporary models directory");
    let path = directory.path().join("model.bin");
    let bytes = b"verified test weights";
    std::fs::write(&path, bytes).expect("write weights");
    let sha256 = digest(&SHA256, bytes)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let provider = LocalProvider::boot(
        ModelId::new("configured-local-model").expect("model id"),
        2,
        true,
        WeightArtifact {
            path: path.clone(),
            url: "https://models.example/model.bin".to_owned(),
            sha256,
        },
        directory.path(),
        CheckedWeightsEngine {
            expected: bytes.to_vec(),
        },
    )
    .expect("verified provider");
    std::fs::write(path, b"swapped after boot").expect("replace path contents");
    provider
        .embed(&["one sentence"])
        .await
        .expect("inference uses verified bytes");
}

#[cfg(unix)]
#[test]
fn local_provider_rejects_a_symlinked_artifact() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().expect("temporary models directory");
    let outside = tempfile::NamedTempFile::new().expect("outside weights");
    let path = directory.path().join("model.bin");
    symlink(outside.path(), &path).expect("symlink fixture");
    let artifact = WeightArtifact {
        path,
        url: "https://models.example/model.bin".to_owned(),
        sha256: "0".repeat(64),
    };
    assert!(
        LocalProvider::boot(
            ModelId::new("configured-local-model").expect("model id"),
            2,
            true,
            artifact,
            directory.path(),
            FixedEngine,
        )
        .is_err()
    );
}

#[test]
fn weight_artifacts_reject_traversal_and_cleartext_urls() {
    let models = PathBuf::from("/srv/aicortex/models");
    let digest = "0".repeat(64);
    let traversal = WeightArtifact {
        path: models.join("../outside/model.bin"),
        url: "https://models.example/model.bin".to_owned(),
        sha256: digest.clone(),
    };
    assert!(traversal.validate(&models).is_err());
    let cleartext = WeightArtifact {
        path: models.join("model.bin"),
        url: "http://models.example/model.bin".to_owned(),
        sha256: digest,
    };
    assert!(cleartext.validate(&models).is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_workers_commit_one_scoped_embedding() {
    let fixture = Fixture::migrated().await;
    let store = fixture.handle();
    let model = test_model(true);
    activate(&store, &model).await;
    let memory = test_memory("alice", "One complete sentence.", 2);
    insert_memory(&store, "scope-a", &memory).await;
    let mut txn = TxnBuilder::new();
    stage_embedding(&mut txn, "scope-a", memory.id, &model, UnixSeconds::new(2))
        .expect("work stages");
    store
        .txn(txn.into_statements())
        .await
        .expect("work commits");

    let first = worker(TestProvider { fail: false }, "worker-a", 3);
    let second = worker(TestProvider { fail: false }, "worker-b", 3);
    let (left, right) = tokio::join!(
        first.drain(&store, UnixSeconds::new(3)),
        second.drain(&store, UnixSeconds::new(3))
    );
    let completed = left.expect("first drain").completed + right.expect("second drain").completed;
    assert_eq!(completed, 1);
    assert_eq!(
        count(
            &store,
            "SELECT COUNT(*) AS count FROM embedding
             WHERE scope_id = ?1 AND memory_id = ?2 AND model_revision = ?3",
            vec![
                Value::from("scope-a"),
                Value::from(memory.id.to_string()),
                Value::Integer(1),
            ],
        )
        .await,
        1
    );
    let chunks: Vec<ChunkIdRow> = store
        .query_consistent(
            "SELECT chunk_id FROM chunk
             WHERE scope_id = ?1 AND memory_id = ?2 AND model_revision = ?3",
            vec![
                Value::from("scope-a"),
                Value::from(memory.id.to_string()),
                Value::Integer(1),
            ],
        )
        .await
        .expect("worker chunk remains readable");
    let identity = format!("scope-a\u{1f}{}\u{1f}1\u{1f}0", memory.id);
    let expected = identity
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].chunk_id, expected);
    fixture.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn expired_claim_chain_reaches_dead_letter_during_worker_drain() {
    let fixture = Fixture::migrated().await;
    let store = fixture.handle();
    let model = test_model(true);
    activate(&store, &model).await;
    let memory = test_memory("alice", "A crashing worker must spend its retry budget.", 2);
    insert_memory(&store, "scope-a", &memory).await;
    let mut txn = TxnBuilder::new();
    stage_embedding(&mut txn, "scope-a", memory.id, &model, UnixSeconds::new(2))
        .expect("work stages");
    store
        .txn(txn.into_statements())
        .await
        .expect("work commits");

    let processor = embedding_processor(model.revision);
    for (holder, now) in [("crashed-a", 3), ("crashed-b", 8), ("crashed-c", 13)] {
        let claims = Work::next(
            &store,
            aicortex_store::EMBEDDING_NAMESPACE,
            &processor,
            holder,
            Duration::from_secs(5),
            UnixSeconds::new(now),
            1,
        )
        .await
        .expect("expired claim is reclaimed");
        assert_eq!(claims.len(), 1);
    }

    let report = worker(TestProvider { fail: false }, "worker-after-crashes", 3)
        .drain(&store, UnixSeconds::new(18))
        .await
        .expect("worker sweep closes the spent expiry chain");
    assert_eq!(report.dead, 1);
    assert_eq!(report.claimed, 1);
    let health = queue_health(&store, "scope-a", UnixSeconds::new(18))
        .await
        .expect("queue health");
    assert_eq!(health.dead, 1);
    assert_eq!(health.pending, 0);
    fixture.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn erasure_during_inference_cannot_resurrect_derivatives() {
    let fixture = Fixture::migrated().await;
    let store = fixture.handle();
    let model = test_model(true);
    activate(&store, &model).await;
    let memory = test_memory("alice", "Erase while inference is running.", 2);
    let later = test_memory("alice", "The claimed batch must keep draining.", 2);
    insert_memory(&store, "scope-a", &memory).await;
    insert_memory(&store, "scope-a", &later).await;
    let mut txn = TxnBuilder::new();
    stage_embedding(&mut txn, "scope-a", memory.id, &model, UnixSeconds::new(2))
        .expect("work stages");
    stage_embedding(&mut txn, "scope-a", later.id, &model, UnixSeconds::new(2))
        .expect("later work stages");
    store
        .txn(txn.into_statements())
        .await
        .expect("work commits");
    let embedding_worker = EmbeddingWorker::new(
        ErasingProvider {
            store: store.clone(),
            scope_id: "scope-a".to_owned(),
            memory_ids: vec![memory.id, later.id],
            erased: Arc::new(AtomicBool::new(false)),
        },
        model,
        Chunker::new(ChunkConfig {
            threshold_bytes: 256,
            target_bytes: 128,
            overlap_bytes: 16,
            max_chunks_per_memory: 512,
        })
        .expect("chunk config"),
        WorkerConfig {
            batch_size: 2,
            hold_for: Duration::from_secs(5),
            retry: RetryPolicy {
                max_attempts: 3,
                base: Duration::from_secs(1),
                cap: Duration::from_secs(2),
            },
        },
        "erasure-race",
    )
    .expect("worker config");
    let report = embedding_worker
        .drain(&store, UnixSeconds::new(3))
        .await
        .expect("race drains");
    assert_eq!(report.claimed, 1);
    assert_eq!(report.completed, 1);
    for memory_id in [memory.id, later.id] {
        assert_eq!(
            count(
                &store,
                "SELECT COUNT(*) AS count FROM embedding WHERE memory_id = ?1",
                vec![Value::from(memory_id.to_string())],
            )
            .await,
            0
        );
        assert_eq!(
            count(
                &store,
                "SELECT COUNT(*) AS count FROM chunk WHERE memory_id = ?1",
                vec![Value::from(memory_id.to_string())],
            )
            .await,
            0
        );
    }
    assert_eq!(
        count(
            &store,
            "SELECT COUNT(*) AS count FROM rahi_processing",
            vec![],
        )
        .await,
        0
    );
    assert_eq!(
        count(
            &store,
            "SELECT COUNT(*) AS count FROM rahi_processing_attempt",
            vec![],
        )
        .await,
        0
    );
    fixture.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn slow_provider_does_not_expire_unprocessed_tail_claims() {
    let fixture = Fixture::migrated().await;
    let store = fixture.handle();
    let model = test_model(true);
    activate(&store, &model).await;
    let memories = [
        test_memory("alice", "First slow embedding.", 2),
        test_memory("alice", "Second slow embedding.", 3),
        test_memory("alice", "Third slow embedding.", 4),
    ];
    let mut txn = TxnBuilder::new();
    for memory in &memories {
        insert_memory(&store, "scope-a", memory).await;
        stage_embedding(&mut txn, "scope-a", memory.id, &model, UnixSeconds::new(2))
            .expect("slow work stages");
    }
    store
        .txn(txn.into_statements())
        .await
        .expect("slow work commits");

    let embedding_worker = EmbeddingWorker::new(
        SlowProvider {
            delay: Duration::from_millis(1_100),
        },
        model,
        Chunker::new(ChunkConfig {
            threshold_bytes: 256,
            target_bytes: 128,
            overlap_bytes: 16,
            max_chunks_per_memory: 512,
        })
        .expect("chunk config"),
        WorkerConfig {
            batch_size: 3,
            hold_for: Duration::from_secs(3),
            retry: RetryPolicy {
                max_attempts: 2,
                base: Duration::from_secs(1),
                cap: Duration::from_secs(2),
            },
        },
        "slow-worker",
    )
    .expect("worker config");
    let report = embedding_worker
        .drain(&store, UnixSeconds::new(10))
        .await
        .expect("slow batch drains");
    assert_eq!((report.claimed, report.completed), (3, 3));
    assert_eq!((report.failed, report.dead), (0, 0));
    assert_eq!(
        count(
            &store,
            "SELECT COUNT(*) AS count FROM rahi_processing_attempt
             WHERE attempt = 1 AND outcome = 'done'",
            vec![],
        )
        .await,
        3
    );
    fixture.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn quarantine_during_inference_moves_work_to_the_requeueable_dead_letter() {
    let fixture = Fixture::migrated().await;
    let store = fixture.handle();
    let model = test_model(true);
    activate(&store, &model).await;
    let memory = test_memory("alice", "Quarantine wins the derivative commit race.", 2);
    insert_memory(&store, "scope-a", &memory).await;
    let mut txn = TxnBuilder::new();
    stage_embedding(&mut txn, "scope-a", memory.id, &model, UnixSeconds::new(2))
        .expect("work stages");
    store
        .txn(txn.into_statements())
        .await
        .expect("work commits");
    let embedding_worker = EmbeddingWorker::new(
        QuarantiningProvider {
            store: store.clone(),
            memory_id: memory.id,
        },
        model,
        Chunker::new(ChunkConfig {
            threshold_bytes: 256,
            target_bytes: 128,
            overlap_bytes: 16,
            max_chunks_per_memory: 512,
        })
        .expect("chunk config"),
        WorkerConfig {
            batch_size: 1,
            hold_for: Duration::from_secs(5),
            retry: RetryPolicy {
                max_attempts: 3,
                base: Duration::from_secs(1),
                cap: Duration::from_secs(2),
            },
        },
        "quarantine-race",
    )
    .expect("worker config");

    let report = embedding_worker
        .drain(&store, UnixSeconds::new(3))
        .await
        .expect("quarantine race drains");
    assert_eq!(
        (report.claimed, report.completed, report.quarantined),
        (1, 0, 1)
    );
    assert_eq!(
        count(
            &store,
            "SELECT COUNT(*) AS count FROM embedding WHERE memory_id = ?1",
            vec![Value::from(memory.id.to_string())],
        )
        .await,
        0
    );
    let health = queue_health(&store, "scope-a", UnixSeconds::new(3))
        .await
        .expect("queue health reads");
    assert_eq!((health.pending, health.dead, health.quarantined), (0, 0, 1));
    fixture.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn activation_during_inference_cannot_restore_stale_revision_vectors() {
    let fixture = Fixture::migrated().await;
    let store = fixture.handle();
    let model = test_model(true);
    activate(&store, &model).await;
    let memory = test_memory("alice", "Activation wins the derivative commit race.", 2);
    insert_memory(&store, "scope-a", &memory).await;
    let mut txn = TxnBuilder::new();
    stage_embedding(&mut txn, "scope-a", memory.id, &model, UnixSeconds::new(2))
        .expect("work stages");
    store
        .txn(txn.into_statements())
        .await
        .expect("work commits");
    let replacement = ModelRevision {
        revision: 2,
        first_seen: UnixSeconds::new(3),
        ..model.clone()
    };
    let embedding_worker = EmbeddingWorker::new(
        SwitchingProvider {
            store: store.clone(),
            replacement: replacement.clone(),
        },
        model,
        Chunker::new(ChunkConfig {
            threshold_bytes: 256,
            target_bytes: 128,
            overlap_bytes: 16,
            max_chunks_per_memory: 512,
        })
        .expect("chunk config"),
        WorkerConfig {
            batch_size: 1,
            hold_for: Duration::from_secs(5),
            retry: RetryPolicy {
                max_attempts: 3,
                base: Duration::from_secs(1),
                cap: Duration::from_secs(2),
            },
        },
        "activation-race",
    )
    .expect("worker config");

    let report = embedding_worker
        .drain(&store, UnixSeconds::new(3))
        .await
        .expect("activation race drains");
    assert_eq!((report.claimed, report.completed, report.failed), (1, 1, 0));
    assert_eq!(
        count(
            &store,
            "SELECT COUNT(*) AS count FROM embedding WHERE memory_id = ?1 AND model_revision = 1",
            vec![Value::from(memory.id.to_string())],
        )
        .await,
        0
    );
    assert_eq!(
        ModelRegistry::active(&store).await.expect("active model"),
        Some(replacement)
    );
    fixture.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deactivation_during_inference_defers_until_the_revision_is_reactivated() {
    let fixture = Fixture::migrated().await;
    let store = fixture.handle();
    let model = test_model(true);
    activate(&store, &model).await;
    let memory = test_memory("alice", "A temporary deactivation preserves its work.", 2);
    insert_memory(&store, "scope-a", &memory).await;
    let mut txn = TxnBuilder::new();
    stage_embedding(&mut txn, "scope-a", memory.id, &model, UnixSeconds::new(2))
        .expect("work stages");
    store
        .txn(txn.into_statements())
        .await
        .expect("work commits");
    let embedding_worker = EmbeddingWorker::new(
        DeactivatingProvider {
            store: store.clone(),
            deactivated: Arc::new(AtomicBool::new(false)),
        },
        model.clone(),
        Chunker::new(ChunkConfig {
            threshold_bytes: 256,
            target_bytes: 128,
            overlap_bytes: 16,
            max_chunks_per_memory: 512,
        })
        .expect("chunk config"),
        WorkerConfig {
            batch_size: 1,
            hold_for: Duration::from_secs(5),
            retry: RetryPolicy {
                max_attempts: 1,
                base: Duration::from_secs(1),
                cap: Duration::from_secs(2),
            },
        },
        "deactivation-race",
    )
    .expect("worker config");

    let deferred = embedding_worker
        .drain(&store, UnixSeconds::new(3))
        .await
        .expect("deactivation defers the claim");
    assert_eq!(
        (deferred.claimed, deferred.completed, deferred.failed),
        (1, 0, 0)
    );
    assert_eq!(
        count(
            &store,
            "SELECT COUNT(*) AS count FROM rahi_processing WHERE state = 'pending'",
            vec![],
        )
        .await,
        1
    );
    activate(&store, &model).await;
    let completed = embedding_worker
        .drain(&store, UnixSeconds::new(9))
        .await
        .expect("reactivated revision reclaims and completes work");
    assert_eq!(
        (completed.claimed, completed.completed, completed.failed),
        (1, 1, 0)
    );
    assert_eq!(
        count(
            &store,
            "SELECT COUNT(*) AS count FROM embedding WHERE memory_id = ?1 AND model_revision = 1",
            vec![Value::from(memory.id.to_string())],
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &store,
            "SELECT COUNT(*) AS count FROM rahi_processing WHERE state = 'dead'",
            vec![],
        )
        .await,
        0
    );
    fixture.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deactivation_does_not_consume_the_provider_failure_budget() {
    let fixture = Fixture::migrated().await;
    let store = fixture.handle();
    let model = test_model(true);
    activate(&store, &model).await;
    let memory = test_memory(
        "alice",
        "Administrative deferral is not a provider failure.",
        2,
    );
    insert_memory(&store, "scope-a", &memory).await;
    let mut txn = TxnBuilder::new();
    stage_embedding(&mut txn, "scope-a", memory.id, &model, UnixSeconds::new(2))
        .expect("work stages");
    store
        .txn(txn.into_statements())
        .await
        .expect("work commits");
    let deactivating = EmbeddingWorker::new(
        DeactivatingProvider {
            store: store.clone(),
            deactivated: Arc::new(AtomicBool::new(false)),
        },
        model.clone(),
        Chunker::new(ChunkConfig {
            threshold_bytes: 256,
            target_bytes: 128,
            overlap_bytes: 16,
            max_chunks_per_memory: 512,
        })
        .expect("chunk config"),
        WorkerConfig {
            batch_size: 1,
            hold_for: Duration::from_secs(5),
            retry: RetryPolicy {
                max_attempts: 2,
                base: Duration::from_secs(1),
                cap: Duration::from_secs(2),
            },
        },
        "deactivation-budget",
    )
    .expect("worker config");

    deactivating
        .drain(&store, UnixSeconds::new(3))
        .await
        .expect("deactivation defers the claim");
    activate(&store, &model).await;
    let failing = worker(TestProvider { fail: true }, "failure-budget", 2);
    let first = failing
        .drain(&store, UnixSeconds::new(9))
        .await
        .expect("first provider failure records");
    assert_eq!((first.failed, first.dead), (1, 0));
    let second = failing
        .drain(&store, UnixSeconds::new(12))
        .await
        .expect("second provider failure records");
    assert_eq!((second.failed, second.dead), (0, 1));
    assert_eq!(
        count(
            &store,
            "SELECT COUNT(*) AS count FROM rahi_processing_attempt
             WHERE error_class = 'upstream'",
            vec![],
        )
        .await,
        2
    );
    fixture.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn terminal_memory_work_stays_recoverable_without_embedding() {
    for state in ["erased", "quarantined", "missing"] {
        let fixture = Fixture::migrated().await;
        let store = fixture.handle();
        let model = test_model(true);
        activate(&store, &model).await;
        let memory = test_memory("alice", "Pending work is cancelled cleanly.", 2);
        insert_memory(&store, "scope-a", &memory).await;
        let mut txn = TxnBuilder::new();
        stage_embedding(&mut txn, "scope-a", memory.id, &model, UnixSeconds::new(2))
            .expect("work stages");
        store
            .txn(txn.into_statements())
            .await
            .expect("work commits");
        let sql = match state {
            "erased" => {
                "UPDATE memory SET status = 'erased', record = 'not json' WHERE scope_id = ?1 AND id = ?2"
            }
            "quarantined" => {
                "UPDATE memory SET status = 'quarantined', record = 'not json' WHERE scope_id = ?1 AND id = ?2"
            }
            "missing" => "DELETE FROM memory WHERE scope_id = ?1 AND id = ?2",
            _ => unreachable!("the test lists every terminal state"),
        };
        store
            .execute(
                sql,
                vec![Value::from("scope-a"), Value::from(memory.id.to_string())],
            )
            .await
            .expect("memory state changes");
        let report = worker(TestProvider { fail: false }, "terminal-worker", 2)
            .drain(&store, UnixSeconds::new(3))
            .await
            .expect("terminal work drains");
        if state == "quarantined" {
            assert_eq!(
                (report.completed, report.dead, report.quarantined),
                (0, 0, 1)
            );
        } else {
            assert_eq!(
                (report.completed, report.dead, report.quarantined),
                (1, 0, 0)
            );
        }
        assert_eq!(
            count(
                &store,
                "SELECT COUNT(*) AS count FROM embedding WHERE memory_id = ?1",
                vec![Value::from(memory.id.to_string())],
            )
            .await,
            0
        );
        let health = queue_health(&store, "scope-a", UnixSeconds::new(3))
            .await
            .expect("health reads");
        assert_eq!(health.dead, 0);
        assert_eq!(health.quarantined, u64::from(state == "quarantined"));
        if state == "quarantined" {
            let preflight = EmbeddingPreflight::read(&store, "scope-a", UnixSeconds::new(3))
                .await
                .expect("quarantine preflight reads");
            assert!(preflight.readiness_warning().is_none());
            assert!(preflight.to_string().contains("quarantined=1"));
            let coverage = ModelRegistry::coverage(&store, "scope-a")
                .await
                .expect("coverage excludes quarantined memories");
            assert_eq!((coverage[0].embedded, coverage[0].total), (0, 0));

            store
                .execute(
                    "UPDATE memory SET status = 'active', record = ?3 WHERE scope_id = ?1 AND id = ?2",
                    vec![
                        Value::from("scope-a"),
                        Value::from(memory.id.to_string()),
                        Value::from(serde_json::to_string(&memory).expect("memory serializes")),
                    ],
                )
                .await
                .expect("review admits the memory");
            let admitted_health = queue_health(&store, "scope-a", UnixSeconds::new(4))
                .await
                .expect("admitted dead letter remains visible");
            assert_eq!((admitted_health.dead, admitted_health.quarantined), (1, 0));
            let admitted_preflight =
                EmbeddingPreflight::read(&store, "scope-a", UnixSeconds::new(4))
                    .await
                    .expect("admitted preflight reads");
            assert!(admitted_preflight.readiness_warning().is_some());
            let mut requeue = TxnBuilder::new();
            Work::requeue(
                &mut requeue,
                &embedding_work_key("scope-a", memory.id, &model).expect("work key"),
                UnixSeconds::new(4),
            );
            store
                .txn(requeue.into_statements())
                .await
                .expect("admission requeues the terminal work");
            let admitted = worker(TestProvider { fail: false }, "admitted-worker", 3)
                .drain(&store, UnixSeconds::new(5))
                .await
                .expect("admitted memory embeds");
            assert_eq!((admitted.completed, admitted.dead), (1, 0));
            assert_eq!(
                count(
                    &store,
                    "SELECT COUNT(*) AS count FROM embedding WHERE memory_id = ?1",
                    vec![Value::from(memory.id.to_string())],
                )
                .await,
                1
            );
        }
        fixture.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn derivative_commit_failure_is_recorded_as_an_item_failure() {
    let fixture = Fixture::migrated().await;
    let store = fixture.handle();
    let model = test_model(true);
    activate(&store, &model).await;
    let memory = test_memory("alice", "The derivative transaction will be rejected.", 2);
    insert_memory(&store, "scope-a", &memory).await;
    let mut txn = TxnBuilder::new();
    stage_embedding(&mut txn, "scope-a", memory.id, &model, UnixSeconds::new(2))
        .expect("work stages");
    store
        .txn(txn.into_statements())
        .await
        .expect("work commits");
    store
        .txn(vec![Statement::new(
            "CREATE TRIGGER reject_embedding BEFORE INSERT ON embedding
             BEGIN SELECT RAISE(FAIL, 'injected embedding commit failure'); END",
        )])
        .await
        .expect("failure trigger installs");

    let report = worker(TestProvider { fail: false }, "commit-failure-worker", 2)
        .drain(&store, UnixSeconds::new(3))
        .await
        .expect("commit failure is recorded without aborting the drain");
    assert_eq!((report.claimed, report.completed), (1, 0));
    assert_eq!((report.failed, report.dead, report.quarantined), (1, 0, 0));
    assert_eq!(
        count(
            &store,
            "SELECT COUNT(*) AS count FROM embedding WHERE memory_id = ?1",
            vec![Value::from(memory.id.to_string())],
        )
        .await,
        0
    );
    let health = queue_health(&store, "scope-a", UnixSeconds::new(3))
        .await
        .expect("failed item remains visible");
    assert_eq!((health.pending, health.dead, health.quarantined), (1, 0, 0));
    fixture.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn oversized_derivative_payload_is_recorded_as_an_item_failure() {
    let fixture = Fixture::migrated().await;
    let store = fixture.handle();
    let model = ModelRevision {
        model_id: ModelId::new("oversized-model").expect("model id"),
        revision: 1,
        dims: u16::MAX,
        normalized: false,
        first_seen: UnixSeconds::new(1),
        active: true,
    };
    activate(&store, &model).await;
    let memory = test_memory(
        "alice",
        "This body is deliberately split into enough chunks to exceed the store entry bound.",
        2,
    );
    insert_memory(&store, "scope-a", &memory).await;
    let mut txn = TxnBuilder::new();
    stage_embedding(&mut txn, "scope-a", memory.id, &model, UnixSeconds::new(2))
        .expect("work stages");
    store
        .txn(txn.into_statements())
        .await
        .expect("work commits");
    let provider_called = Arc::new(AtomicBool::new(false));
    let embedding_worker = EmbeddingWorker::new(
        OversizedProvider {
            called: Arc::clone(&provider_called),
        },
        model,
        Chunker::new(ChunkConfig {
            threshold_bytes: 16,
            target_bytes: 8,
            overlap_bytes: 0,
            max_chunks_per_memory: 32,
        })
        .expect("chunk config"),
        WorkerConfig {
            batch_size: 1,
            hold_for: Duration::from_secs(5),
            retry: RetryPolicy {
                max_attempts: 2,
                base: Duration::from_secs(1),
                cap: Duration::from_secs(2),
            },
        },
        "oversized-worker",
    )
    .expect("worker config");

    let report = embedding_worker
        .drain(&store, UnixSeconds::new(3))
        .await
        .expect("oversized payload is recorded without aborting the drain");
    assert_eq!((report.claimed, report.completed), (1, 0));
    assert_eq!((report.failed, report.dead, report.quarantined), (1, 0, 0));
    assert!(
        !provider_called.load(Ordering::SeqCst),
        "oversized derivative work must be rejected before provider egress"
    );
    assert_eq!(
        count(
            &store,
            "SELECT COUNT(*) AS count FROM embedding WHERE memory_id = ?1",
            vec![Value::from(memory.id.to_string())],
        )
        .await,
        0
    );
    let health = queue_health(&store, "scope-a", UnixSeconds::new(3))
        .await
        .expect("failed item remains visible");
    assert_eq!((health.pending, health.dead, health.quarantined), (1, 0, 0));
    fixture.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_worker_never_relabels_work_from_another_model_revision() {
    let fixture = Fixture::migrated().await;
    let store = fixture.handle();
    let model = ModelRevision {
        model_id: ModelId::new("test-model").expect("model id"),
        revision: 2,
        dims: 2,
        normalized: true,
        first_seen: UnixSeconds::new(1),
        active: true,
    };
    activate(&store, &model).await;
    let memory = test_memory("alice", "Revision identity must survive the queue.", 2);
    insert_memory(&store, "scope-a", &memory).await;
    let mut txn = TxnBuilder::new();
    stage_embedding(&mut txn, "scope-a", memory.id, &model, UnixSeconds::new(2))
        .expect("work stages");
    store
        .txn(txn.into_statements())
        .await
        .expect("work commits");

    let report = worker(TestProvider { fail: false }, "old-worker", 3)
        .drain(&store, UnixSeconds::new(3))
        .await
        .expect("mismatch is recorded through the queue");
    assert_eq!(report.completed, 0);
    assert_eq!(report.failed, 0);
    assert_eq!(report.claimed, 0);
    let health = queue_health(&store, "scope-a", UnixSeconds::new(3))
        .await
        .expect("queue health reads");
    assert_eq!(health.pending, 1);
    assert_eq!(health.dead, 0);
    let preflight = EmbeddingPreflight::read(&store, "scope-a", UnixSeconds::new(3))
        .await
        .expect("preflight reads");
    assert!(preflight.to_string().contains("oldest_pending_seconds=1"));
    let matching = worker_for_model(TestProvider { fail: false }, model, "new-worker", 3)
        .drain(&store, UnixSeconds::new(3))
        .await
        .expect("matching worker drains");
    assert_eq!(matching.completed, 1);
    assert_eq!(
        count(
            &store,
            "SELECT COUNT(*) AS count FROM embedding WHERE memory_id = ?1",
            vec![Value::from(memory.id.to_string())],
        )
        .await,
        1
    );
    fixture.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_worker_never_completes_same_revision_work_for_another_model() {
    let fixture = Fixture::migrated().await;
    let store = fixture.handle();
    let active_model = test_model(true);
    activate(&store, &active_model).await;
    let memory = test_memory("alice", "Model identity must survive the queue.", 2);
    insert_memory(&store, "scope-a", &memory).await;
    let mut txn = TxnBuilder::new();
    stage_embedding(
        &mut txn,
        "scope-a",
        memory.id,
        &active_model,
        UnixSeconds::new(2),
    )
    .expect("work stages");
    store
        .txn(txn.into_statements())
        .await
        .expect("work commits");

    let other_model = ModelRevision {
        model_id: ModelId::new("other-model").expect("model id"),
        ..active_model
    };
    let mismatched = EmbeddingWorker::new(
        OtherProvider,
        other_model,
        Chunker::new(ChunkConfig {
            threshold_bytes: 256,
            target_bytes: 128,
            overlap_bytes: 16,
            max_chunks_per_memory: 512,
        })
        .expect("chunk config"),
        WorkerConfig {
            batch_size: 16,
            hold_for: Duration::from_secs(5),
            retry: RetryPolicy {
                max_attempts: 3,
                base: Duration::from_secs(1),
                cap: Duration::from_secs(2),
            },
        },
        "other-worker",
    )
    .expect("worker config");
    let report = mismatched
        .drain(&store, UnixSeconds::new(3))
        .await
        .expect("mismatch leaves the queue untouched");
    assert_eq!(report.claimed, 0);
    assert_eq!(report.completed, 0);
    assert_eq!(report.failed, 0);
    assert_eq!(
        count(
            &store,
            "SELECT attempt AS count FROM rahi_processing LIMIT 1",
            vec![],
        )
        .await,
        0
    );
    assert_eq!(
        count(
            &store,
            "SELECT COUNT(*) AS count FROM embedding WHERE memory_id = ?1",
            vec![Value::from(memory.id.to_string())],
        )
        .await,
        0
    );
    let health = queue_health(&store, "scope-a", UnixSeconds::new(3))
        .await
        .expect("queue health reads");
    assert_eq!(health.pending, 1);
    assert_eq!(health.dead, 0);
    fixture.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_worker_never_claims_a_stored_revision_with_another_vector_layout() {
    let fixture = Fixture::migrated().await;
    let store = fixture.handle();
    let active_model = test_model(true);
    activate(&store, &active_model).await;
    let memory = test_memory("alice", "Vector layout is part of model identity.", 2);
    insert_memory(&store, "scope-a", &memory).await;
    let mut txn = TxnBuilder::new();
    stage_embedding(
        &mut txn,
        "scope-a",
        memory.id,
        &active_model,
        UnixSeconds::new(2),
    )
    .expect("work stages");
    store
        .txn(txn.into_statements())
        .await
        .expect("work commits");

    for (dims, normalized) in [(3, true), (2, false)] {
        let supplied_model = ModelRevision {
            dims,
            normalized,
            ..active_model.clone()
        };
        let mismatched = EmbeddingWorker::new(
            LayoutProvider { dims, normalized },
            supplied_model,
            Chunker::new(ChunkConfig {
                threshold_bytes: 256,
                target_bytes: 128,
                overlap_bytes: 16,
                max_chunks_per_memory: 512,
            })
            .expect("chunk config"),
            WorkerConfig {
                batch_size: 16,
                hold_for: Duration::from_secs(5),
                retry: RetryPolicy {
                    max_attempts: 3,
                    base: Duration::from_secs(1),
                    cap: Duration::from_secs(2),
                },
            },
            "layout-worker",
        )
        .expect("worker config");
        let report = mismatched
            .drain(&store, UnixSeconds::new(3))
            .await
            .expect("layout mismatch leaves queue untouched");
        assert_eq!((report.claimed, report.completed, report.failed), (0, 0, 0));
    }
    let health = queue_health(&store, "scope-a", UnixSeconds::new(3))
        .await
        .expect("queue health reads");
    assert_eq!((health.pending, health.dead), (1, 0));
    fixture.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn queue_health_excludes_foreign_scopes_namespaces_and_non_revision_processors() {
    let fixture = Fixture::migrated().await;
    let store = fixture.handle();
    let mut txn = TxnBuilder::new();
    for (namespace, processor, key) in [
        ("foreign.memory", "embed.r1", "foreign"),
        (
            aicortex_store::EMBEDDING_NAMESPACE,
            "embed.retry",
            "nonnumeric",
        ),
        (
            aicortex_store::EMBEDDING_NAMESPACE,
            "embed.r1.extra",
            "numeric-prefix",
        ),
    ] {
        let receipt = ReceiptKey::new("scope-a", namespace, key).expect("receipt key");
        let work = ProcessingKey::new(receipt, 1, processor, "test").expect("processing key");
        Work::stage_work(&mut txn, &work, UnixSeconds::new(2));
    }
    let foreign_receipt = ReceiptKey::new(
        "scope-b",
        aicortex_store::EMBEDDING_NAMESPACE,
        MemoryId::now_v7().to_string(),
    )
    .expect("foreign receipt key");
    let foreign_work =
        ProcessingKey::new(foreign_receipt, 1, "embed.r1", "test").expect("foreign processing key");
    Work::stage_work(&mut txn, &foreign_work, UnixSeconds::new(2));
    store
        .txn(txn.into_statements())
        .await
        .expect("unrelated work commits");

    let health = queue_health(&store, "scope-a", UnixSeconds::new(3))
        .await
        .expect("queue health reads");
    assert_eq!(health, Default::default());
    fixture.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn preflight_warns_when_live_memory_has_no_active_model() {
    let fixture = Fixture::migrated().await;
    let store = fixture.handle();
    let memory = test_memory("alice", "Unembedded memory must stay visible.", 2);
    insert_memory(&store, "scope-a", &memory).await;

    let preflight = EmbeddingPreflight::read(&store, "scope-a", UnixSeconds::new(3))
        .await
        .expect("preflight reads without a model");
    assert_eq!(preflight.live_memories, 1);
    assert!(preflight.coverage.is_empty());
    assert_eq!(
        preflight.readiness_warning(),
        Some("live memories have no active embedding model")
    );
    assert!(preflight.to_string().contains("live=1 coverage=[]"));
    fixture.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inactive_revision_work_completes_before_active_reembedding() {
    let fixture = Fixture::migrated().await;
    let store = fixture.handle();
    let old_model = test_model(true);
    activate(&store, &old_model).await;
    let memory = test_memory("alice", "A model switch must preserve work.", 2);
    insert_memory(&store, "scope-a", &memory).await;
    let mut txn = TxnBuilder::new();
    stage_embedding(
        &mut txn,
        "scope-a",
        memory.id,
        &old_model,
        UnixSeconds::new(2),
    )
    .expect("old revision work stages");
    store
        .txn(txn.into_statements())
        .await
        .expect("old revision work commits");

    let active_model = ModelRevision {
        revision: 2,
        first_seen: UnixSeconds::new(3),
        ..test_model(true)
    };
    activate(&store, &active_model).await;
    let transition_health = queue_health(&store, "scope-a", UnixSeconds::new(3))
        .await
        .expect("old revision work remains visible during the transition");
    assert_eq!(transition_health.pending, 1);
    let old_worker = worker_for_model(TestProvider { fail: false }, old_model, "old-worker", 3);
    let old_report = old_worker
        .drain(&store, UnixSeconds::new(4))
        .await
        .expect("inactive worker drains stale work as a terminal no-op");
    assert_eq!(old_report.completed, 1);
    let health = queue_health(&store, "scope-a", UnixSeconds::new(4))
        .await
        .expect("inactive revision no longer affects readiness");
    assert_eq!(health.pending, 0);
    assert_eq!(health.dead, 0);

    let batch = stage_reembedding_batch(
        &store,
        "scope-a",
        &active_model,
        None,
        10,
        UnixSeconds::new(5),
    )
    .await
    .expect("model activation stages bounded re-embedding");
    assert_eq!(batch.staged, 1);
    let active_worker = worker_for_model(
        TestProvider { fail: false },
        active_model,
        "active-worker",
        3,
    );
    let active_report = active_worker
        .drain(&store, UnixSeconds::new(6))
        .await
        .expect("active worker drains re-embedding work");
    assert_eq!(active_report.completed, 1);
    let health = queue_health(&store, "scope-a", UnixSeconds::new(6))
        .await
        .expect("active revision queue becomes ready");
    assert_eq!(health.pending, 0);
    assert_eq!(health.dead, 0);
    assert_eq!(
        count(
            &store,
            "SELECT COUNT(*) AS count FROM embedding WHERE memory_id = ?1 AND model_revision = 2",
            vec![Value::from(memory.id.to_string())],
        )
        .await,
        1
    );
    fixture.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stale_reembedding_selection_cannot_restore_erased_work() {
    let fixture = Fixture::migrated().await;
    let store = fixture.handle();
    let model = test_model(true);
    activate(&store, &model).await;
    let memory = test_memory("alice", "Erasure wins the staging race.", 2);
    insert_memory(&store, "scope-a", &memory).await;

    let mut stale_batch = TxnBuilder::new();
    stage_live_embedding(
        &mut stale_batch,
        "scope-a",
        memory.id,
        &model,
        UnixSeconds::new(3),
    )
    .expect("live selection builds its staging transaction");
    store
        .execute(
            "UPDATE memory SET status = 'erased' WHERE scope_id = ?1 AND id = ?2",
            vec![Value::from("scope-a"), Value::from(memory.id.to_string())],
        )
        .await
        .expect("erasure commits before the stale staging transaction");

    assert!(
        store.txn(stale_batch.into_statements()).await.is_err(),
        "the commit-time liveness guard must abort stale staging"
    );
    assert_eq!(
        count(
            &store,
            "SELECT COUNT(*) AS count FROM rahi_processing WHERE namespace = ?1 AND key = ?2",
            vec![
                Value::from(aicortex_store::EMBEDDING_NAMESPACE),
                Value::from(memory.id.to_string()),
            ],
        )
        .await,
        0
    );
    fixture.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn provider_failures_retry_then_reach_dead_letter_and_preflight_warns() {
    let fixture = Fixture::migrated().await;
    let store = fixture.handle();
    let model = test_model(true);
    activate(&store, &model).await;
    let memory = test_memory("alice", "This provider will fail.", 2);
    insert_memory(&store, "scope-a", &memory).await;
    let mut txn = TxnBuilder::new();
    stage_embedding(&mut txn, "scope-a", memory.id, &model, UnixSeconds::new(2))
        .expect("work stages");
    store
        .txn(txn.into_statements())
        .await
        .expect("work commits");

    let failing = worker(TestProvider { fail: true }, "failing-worker", 2);
    let first = failing
        .drain(&store, UnixSeconds::new(3))
        .await
        .expect("first failure records");
    assert_eq!(first.failed, 1);
    let second = failing
        .drain(&store, UnixSeconds::new(5))
        .await
        .expect("second failure records");
    assert_eq!(second.dead, 1);
    let health = queue_health(&store, "scope-a", UnixSeconds::new(5))
        .await
        .expect("health reads");
    assert_eq!(health.dead, 1);
    assert_eq!(health.quarantined, 0);
    let preflight = EmbeddingPreflight::read(&store, "scope-a", UnixSeconds::new(5))
        .await
        .expect("preflight reads");
    assert!(preflight.readiness_warning().is_some());
    assert!(preflight.to_string().contains("dead=1"));
    assert!(preflight.to_string().contains("warning="));
    let restaged =
        stage_reembedding_batch(&store, "scope-a", &model, None, 10, UnixSeconds::new(6))
            .await
            .expect("dead work remains an operator-visible terminal item");
    assert_eq!(restaged.staged, 0);
    assert_eq!(restaged.cursor, Some(memory.id));

    let replacement = ModelRevision {
        revision: 2,
        first_seen: UnixSeconds::new(6),
        ..test_model(true)
    };
    activate(&store, &replacement).await;
    let replacement_health = queue_health(&store, "scope-a", UnixSeconds::new(6))
        .await
        .expect("health reads after model activation");
    assert_eq!(replacement_health.dead, 1);
    let replacement_preflight = EmbeddingPreflight::read(&store, "scope-a", UnixSeconds::new(6))
        .await
        .expect("preflight reads after model activation");
    assert!(replacement_preflight.readiness_warning().is_some());
    assert!(replacement_preflight.to_string().contains("dead=1"));
    fixture.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scoped_derivatives_and_coverage_never_cross_scope_boundaries() {
    let fixture = Fixture::migrated().await;
    let store = fixture.handle();
    let model = test_model(true);
    activate(&store, &model).await;
    let alice = test_memory("alice", "Alice memory.", 2);
    let bob = test_memory("bob", "Bob memory.", 2);
    insert_memory(&store, "scope-a", &alice).await;
    insert_memory(&store, "scope-b", &bob).await;
    let mut txn = TxnBuilder::new();
    stage_embedding(&mut txn, "scope-a", alice.id, &model, UnixSeconds::new(2))
        .expect("Alice work stages");
    stage_embedding(&mut txn, "scope-b", bob.id, &model, UnixSeconds::new(2))
        .expect("Bob work stages");
    store
        .txn(txn.into_statements())
        .await
        .expect("work commits");
    let embedding_worker = worker(TestProvider { fail: false }, "scope-worker", 3);
    let report = embedding_worker
        .drain(&store, UnixSeconds::new(3))
        .await
        .expect("both scopes drain");
    assert_eq!(report.completed, 2);

    let alice_coverage = ModelRegistry::coverage(&store, "scope-a")
        .await
        .expect("Alice coverage reads");
    let bob_coverage = ModelRegistry::coverage(&store, "scope-b")
        .await
        .expect("Bob coverage reads");
    assert_eq!(
        (alice_coverage[0].embedded, alice_coverage[0].total),
        (1, 1)
    );
    assert_eq!((bob_coverage[0].embedded, bob_coverage[0].total), (1, 1));

    for statement in [
        "INSERT INTO chunk
            (chunk_id, scope_id, memory_id, model_revision, ordinal, byte_start, byte_end)
         VALUES ('orphan', 'scope-a', 'missing', 1, 0, 0, 1)",
        "INSERT INTO embedding
            (scope_id, memory_id, model_id, model_revision, chunk_ordinal,
             dims, normalized, vector, updated)
         VALUES ('scope-a', 'missing', 'test-model', 1, 0, 2, 1,
             X'0000803F00000000', 3)",
    ] {
        store
            .execute(statement.to_owned(), vec![])
            .await
            .expect("orphan derivative fixture commits");
    }
    let alice_coverage = ModelRegistry::coverage(&store, "scope-a")
        .await
        .expect("coverage ignores orphan derivatives");
    assert_eq!(
        (alice_coverage[0].embedded, alice_coverage[0].total),
        (1, 1)
    );

    let charlie = test_memory("charlie", "Queued in Bob's scope.", 3);
    insert_memory(&store, "scope-b", &charlie).await;
    let mut stale = TxnBuilder::new();
    stage_embedding(
        &mut stale,
        "scope-b",
        charlie.id,
        &model,
        UnixSeconds::new(3),
    )
    .expect("other-scope old-revision work stages");
    store
        .txn(stale.into_statements())
        .await
        .expect("other-scope old-revision work commits");

    let model_two = ModelRevision {
        revision: 2,
        first_seen: UnixSeconds::new(4),
        ..test_model(true)
    };
    activate(&store, &model_two).await;
    let mut txn = TxnBuilder::new();
    stage_embedding(
        &mut txn,
        "scope-a",
        alice.id,
        &model_two,
        UnixSeconds::new(4),
    )
    .expect("Alice revision two stages");
    stage_embedding(&mut txn, "scope-b", bob.id, &model_two, UnixSeconds::new(4))
        .expect("Bob revision two stages");
    store
        .txn(txn.into_statements())
        .await
        .expect("revision two work commits");
    let report = worker_for_model(
        TestProvider { fail: false },
        model_two,
        "scope-worker-two",
        3,
    )
    .drain(&store, UnixSeconds::new(5))
    .await
    .expect("revision two drains");
    assert_eq!(report.completed, 2);
    ModelRegistry::drop_revision(&store, "scope-a", 1)
        .await
        .expect("scoped drop ignores another scope's queue");
    let missing = ModelRegistry::drop_revision(&store, "scope-a", 99)
        .await
        .expect_err("an unknown revision cannot report a successful drop");
    assert!(matches!(missing, Error::Conflict(_)));
    assert_eq!(
        count(
            &store,
            "SELECT COUNT(*) AS count FROM embedding WHERE scope_id = ?1",
            vec![Value::from("scope-a")],
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &store,
            "SELECT COUNT(*) AS count FROM embedding WHERE scope_id = ?1",
            vec![Value::from("scope-b")],
        )
        .await,
        2
    );
    fixture.shutdown().await;
}
