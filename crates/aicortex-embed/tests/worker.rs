#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used
)]

use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::time::Duration;

use aicortex_embed::local::{LocalEngine, LocalProvider, WeightArtifact};
use aicortex_embed::migrations::{EMBEDDING_MIGRATION_VERSION, migration};
use aicortex_embed::provider::{EmbeddingProvider, ModelId, Vector};
use aicortex_embed::registry::{ModelRegistry, ModelRevision};
use aicortex_embed::{
    ChunkConfig, Chunker, EmbeddingPreflight, EmbeddingWorker, WorkerConfig, embedding_work_key,
    queue_health, stage_active_embedding, stage_embedding,
};
use aicortex_types::{
    Actor, ActorId, Importance, Memory, MemoryBody, MemoryId, MemoryKind, MemoryParts, Provenance,
    Scope, SourceRef, SourceSystem, TrustClass,
};
use rahi_store::{
    EncKey, EncKeys, RetryPolicy, Statement, Store, StoreConfig, StoreHandle, StoreSecrets,
    TxnBuilder, Value, coordination_set, receipt_set,
};
use rahi_types::{Error, Sub, UnixSeconds};
use ring::digest::{SHA256, digest};
use serde::Deserialize;

#[derive(Clone, Copy, Debug)]
struct FixedEngine;

impl LocalEngine for FixedEngine {
    async fn infer(&self, _weights: &Path, batch: &[&str]) -> Result<Vec<Vector>, Error> {
        (0..batch.len())
            .map(|_| Vector::new(vec![1.0, 0.0]))
            .collect()
    }
}

#[derive(Clone, Copy, Debug)]
struct TestProvider {
    fail: bool,
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

struct Fixture {
    store: Store,
    _directory: tempfile::TempDir,
}

impl Fixture {
    async fn migrated() -> Self {
        let directory = tempfile::tempdir().expect("temporary store directory");
        let store = Store::open(&store_config(directory.path().join("hiqlite")))
            .await
            .expect("single-voter store opens");
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
        let embedding_migration = migration();
        for statement in embedding_migration.sql.split(';').map(str::trim) {
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
    ModelRegistry::activate(&mut txn, model);
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
    EmbeddingWorker::new(
        provider,
        test_model(true),
        Chunker::new(ChunkConfig {
            threshold_bytes: 256,
            target_bytes: 128,
            overlap_bytes: 16,
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
fn migration_has_exact_tables_and_follows_existing_versions() {
    let migration = migration();
    assert_eq!(EMBEDDING_MIGRATION_VERSION, 9);
    assert_eq!(migration.version, 9);
    for table in ["embedding_model", "chunk", "embedding"] {
        assert!(
            migration
                .sql
                .contains(&format!("CREATE TABLE IF NOT EXISTS {table}"))
        );
    }
    assert!(migration.sql.contains("scope_id TEXT NOT NULL"));
    assert!(
        migration
            .sql
            .contains("PRIMARY KEY (scope_id, memory_id, model_revision")
    );
    assert!(migration.additive);
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
    stage_active_embedding(&mut txn, "scope-a", id, UnixSeconds::new(1))
        .expect("capture work statement");
    let statements = format!("{:?}", txn.statements());
    assert!(statements.contains("SELECT revision FROM embedding_model WHERE active = 1"));
    assert!(statements.contains("ON CONFLICT"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn capture_work_without_an_active_model_aborts_its_transaction() {
    let fixture = Fixture::migrated().await;
    let store = fixture.handle();
    let mut txn = TxnBuilder::new();
    stage_active_embedding(&mut txn, "scope-a", MemoryId::now_v7(), UnixSeconds::new(1))
        .expect("work statement stages");
    assert!(store.txn(txn.into_statements()).await.is_err());
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
    assert_eq!(key.processor, "embed");
    assert_eq!(key.processor_revision, "configured-local-model");

    let mut txn = TxnBuilder::new();
    stage_embedding(&mut txn, "scope-digest", id, &model, UnixSeconds::new(11))
        .expect("first stage");
    stage_embedding(&mut txn, "scope-digest", id, &model, UnixSeconds::new(11))
        .expect("repeat stage");
    assert_eq!(txn.len(), 2);
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
        FixedEngine,
    )
    .expect("verified provider");
    assert_eq!(provider.id().as_str(), "configured-local-model");
    assert_eq!(provider.dims(), 2);
    assert!(provider.normalized());
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
    fixture.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_worker_never_relabels_work_from_another_model_revision() {
    let fixture = Fixture::migrated().await;
    let store = fixture.handle();
    let model = ModelRevision {
        model_id: ModelId::new("next-model").expect("model id"),
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
    assert_eq!(report.failed, 1);
    assert_eq!(
        count(
            &store,
            "SELECT COUNT(*) AS count FROM embedding WHERE memory_id = ?1",
            vec![Value::from(memory.id.to_string())],
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
    let preflight = EmbeddingPreflight::read(&store, "scope-a", UnixSeconds::new(5))
        .await
        .expect("preflight reads");
    assert!(preflight.readiness_warning().is_some());
    assert!(preflight.to_string().contains("dead=1"));
    assert!(preflight.to_string().contains("warning="));
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

    let mut drop_txn = TxnBuilder::new();
    drop_txn.push(Statement::with_params(
        "UPDATE embedding_model SET active = 0 WHERE revision = ?1",
        vec![Value::Integer(1)],
    ));
    ModelRegistry::drop_revision(&mut drop_txn, "scope-a", 1);
    store
        .txn(drop_txn.into_statements())
        .await
        .expect("scoped drop commits");
    assert_eq!(
        count(
            &store,
            "SELECT COUNT(*) AS count FROM embedding WHERE scope_id = ?1",
            vec![Value::from("scope-a")],
        )
        .await,
        0
    );
    assert_eq!(
        count(
            &store,
            "SELECT COUNT(*) AS count FROM embedding WHERE scope_id = ?1",
            vec![Value::from("scope-b")],
        )
        .await,
        1
    );
    fixture.shutdown().await;
}
