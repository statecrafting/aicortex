//! The store fixture the graph tests share (spec 017).
//!
//! A real single-voter rahi node in a throwaway directory migrated with the
//! application schema. Memories are written as rows directly, so a test
//! controls exactly what the extraction worker reads, and embeddings are
//! seeded as chunk and vector rows rather than produced by a provider.
//!
//! Statements use `?n` placeholders, which hiqlite binds by number.

#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::net::{SocketAddr, TcpListener};
use std::path::Path;

use aicortex_graph::{
    DrainReport, EntityId, Extraction, ExtractionConfig, ExtractionWorker, Extractor, KnownEntity,
    PatternExtractor,
};
use aicortex_store::{ScopeId, stage_extraction};
use aicortex_types::{
    Actor, ActorId, Importance, Memory, MemoryBody, MemoryId, MemoryKind, MemoryParts, Provenance,
    Scope, SourceRef, SourceSystem, TrustClass,
};
use rahi_store::{
    EncKey, EncKeys, Statement, Store, StoreConfig, StoreHandle, StoreSecrets, TxnBuilder, Value,
};
use rahi_types::{Sub, UnixSeconds};
use serde::Deserialize;

/// A node in a temporary directory.
pub struct Node {
    pub store: Store,
    pub dir: tempfile::TempDir,
}

impl Node {
    pub fn handle(&self) -> StoreHandle {
        self.store.handle()
    }

    pub async fn shutdown(self) {
        self.store.shutdown().await.expect("the node stops");
    }
}

/// The pattern extractor under another version, for re-extraction tests.
#[derive(Clone, Copy, Debug)]
pub struct Versioned(pub u32);

impl Extractor for Versioned {
    fn name(&self) -> &str {
        PatternExtractor::NAME
    }

    fn version(&self) -> u32 {
        self.0
    }

    fn extract(&self, text: &str, known: &[KnownEntity]) -> Extraction {
        PatternExtractor.extract(text, known)
    }
}

/// A migrated node.
pub async fn migrated() -> Node {
    let mut last = None;
    for _ in 0..5 {
        let dir = tempfile::tempdir().expect("a temporary directory");
        match Store::open(&config(&dir.path().join("hiqlite"))).await {
            Ok(store) => {
                store
                    .handle()
                    .migrate(aicortex_store::migrations())
                    .await
                    .expect("the schema applies");
                store
                    .handle()
                    .migrate_sets(
                        &[],
                        &[rahi_store::coordination_set(), rahi_store::receipt_set()],
                    )
                    .await
                    .expect("the work schema applies");
                return Node { store, dir };
            }
            Err(error) if error.message().contains("Address already in use") => last = Some(error),
            Err(error) => panic!("a single-voter node opens: {error}"),
        }
    }
    panic!("a node opens after bounded port retries: {:?}", last);
}

fn free_addr() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a free port");
    listener.local_addr().expect("the port it took")
}

fn config(data_dir: &Path) -> StoreConfig {
    StoreConfig {
        node_id: 1,
        nodes: Vec::new(),
        data_dir: data_dir.to_path_buf(),
        raft_addr: free_addr(),
        api_addr: free_addr(),
        secrets: StoreSecrets {
            secret_raft: "raft-secret-for-graph-tests".to_owned(),
            secret_api: "api-secret-for-graph-tests".to_owned(),
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
    }
}

/// The scope owned by `name` and its store id.
pub fn scope(name: &str) -> (Scope, String) {
    let scope = Scope::personal(Sub::new(name));
    let id = ScopeId::of(&scope).as_str().to_owned();
    (scope, id)
}

/// Write the scope row and one live memory with `text`; returns its id.
pub async fn add_memory(store: &StoreHandle, scope: &Scope, text: &str, created: u64) -> MemoryId {
    let at = UnixSeconds::new(created);
    let memory = Memory::new(MemoryParts {
        id: MemoryId::now_v7(),
        scope: scope.clone(),
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
    });
    insert(
        store,
        scope,
        &memory,
        &serde_json::to_string(&memory).unwrap(),
    )
    .await;
    memory.id
}

/// Write a live memory row whose record cannot be decoded.
pub async fn add_corrupt_memory(store: &StoreHandle, scope: &Scope) -> MemoryId {
    let id = MemoryId::now_v7();
    let memory = Memory::new(MemoryParts {
        id,
        scope: scope.clone(),
        kind: MemoryKind::Observation,
        body: MemoryBody::text("x"),
        actor: Actor::human(ActorId::new("tester").expect("actor id")),
        provenance: Provenance::captured(
            SourceRef::new(SourceSystem::new("test").expect("source system")),
            UnixSeconds::new(1),
            UnixSeconds::new(1),
        ),
        trust: TrustClass::Assertion,
        importance: Importance::at(UnixSeconds::new(1)).expect("importance"),
        created: UnixSeconds::new(1),
    });
    insert(store, scope, &memory, "{not json").await;
    id
}

async fn insert(store: &StoreHandle, scope: &Scope, memory: &Memory, record: &str) {
    let scope_id = ScopeId::of(scope);
    store
        .txn(vec![
            Statement::with_params(
                "INSERT INTO scope (scope_id, owner, kind, key, created)
                 VALUES (?1, ?2, ?3, ?4, 1700000000) ON CONFLICT DO NOTHING",
                vec![
                    Value::from(scope_id.as_str()),
                    Value::from(scope.owner.as_str()),
                    Value::from(scope.kind.label()),
                    Value::from(ScopeId::key_of(scope)),
                ],
            ),
            Statement::with_params(
                "INSERT INTO memory
                 (id, scope_id, status, kind, trust, fingerprint, schema_version, body_bytes,
                  created, updated, record)
                 VALUES (?1, ?2, 'active', ?3, ?4, ?1, 1, 1, ?5, ?5, ?6)",
                vec![
                    Value::from(memory.id.to_string()),
                    Value::from(scope_id.as_str()),
                    Value::from(memory.kind.label()),
                    Value::from(memory.trust.label()),
                    Value::Integer(i64::try_from(memory.created.get()).unwrap()),
                    Value::from(record),
                ],
            ),
        ])
        .await
        .expect("the memory row commits");
}

/// Register an active two-wide model and one chunk and vector for `memory`
/// covering `bytes` bytes of its text.
pub async fn embed(
    store: &StoreHandle,
    scope_id: &str,
    memory: MemoryId,
    bytes: usize,
    vector: [f32; 2],
) {
    store
        .execute(
            "INSERT INTO embedding_model (model_id, revision, dims, normalized, first_seen, active)
             VALUES ('graph-test', 1, 2, 1, 1700000000, 1) ON CONFLICT DO NOTHING",
            vec![],
        )
        .await
        .expect("the model row");
    let blob: Vec<u8> = vector.iter().flat_map(|v| v.to_le_bytes()).collect();
    store
        .txn(vec![
            Statement::with_params(
                "INSERT INTO chunk
                 (chunk_id, scope_id, memory_id, model_revision, ordinal, byte_start, byte_end)
                 VALUES (?1, ?2, ?3, 1, 0, 0, ?4)",
                vec![
                    Value::from(format!("chunk-{memory}")),
                    Value::from(scope_id),
                    Value::from(memory.to_string()),
                    Value::Integer(i64::try_from(bytes).unwrap()),
                ],
            ),
            Statement::with_params(
                "INSERT INTO embedding
                 (scope_id, memory_id, model_id, model_revision, chunk_ordinal, dims,
                  normalized, vector, updated)
                 VALUES (?1, ?2, 'graph-test', 1, 0, 2, 1, ?3, 1700000000)",
                vec![
                    Value::from(scope_id),
                    Value::from(memory.to_string()),
                    Value::Blob(blob),
                ],
            ),
        ])
        .await
        .expect("the chunk and vector commit");
}

/// Stage extraction of `memory` at `extractor`'s version and drain once.
pub async fn extract_with<E: Extractor + Clone>(
    store: &StoreHandle,
    scope_id: &str,
    memory: MemoryId,
    extractor: E,
    config: ExtractionConfig,
) -> DrainReport {
    let mut txn = TxnBuilder::new();
    stage_extraction(
        &mut txn,
        scope_id,
        memory,
        extractor.name(),
        extractor.version(),
        UnixSeconds::new(1_700_000_001),
    )
    .expect("the work stages");
    store
        .txn(txn.into_statements())
        .await
        .expect("the work commits");
    ExtractionWorker::new(extractor, config, "graph-test-worker")
        .expect("a worker")
        .drain(store, UnixSeconds::new(1_700_000_100))
        .await
        .expect("a drain")
}

/// Extract `memory` with the default extractor.
pub async fn extract(store: &StoreHandle, scope_id: &str, memory: MemoryId) -> DrainReport {
    extract_with(
        store,
        scope_id,
        memory,
        PatternExtractor,
        ExtractionConfig::standard(),
    )
    .await
}

#[derive(Debug, Deserialize)]
struct CountRow {
    count: i64,
}

/// `SELECT COUNT(*) AS count ...` as a number.
pub async fn count(store: &StoreHandle, sql: &str, params: Vec<Value>) -> i64 {
    let rows: Vec<CountRow> = store
        .query_consistent(sql.to_owned(), params)
        .await
        .expect("a count reads");
    rows[0].count
}

#[derive(Debug, Deserialize)]
struct IdRow {
    entity_id: String,
}

/// The entity an alias names, in `scope_id`.
pub async fn entity_named(store: &StoreHandle, scope_id: &str, alias: &str) -> EntityId {
    let rows: Vec<IdRow> = store
        .query_consistent(
            "SELECT COALESCE(e.merged_into, e.entity_id) AS entity_id
             FROM entity_alias a JOIN entity e ON e.entity_id = a.entity_id
             WHERE a.scope_id = ?1 AND a.alias = ?2",
            vec![Value::from(scope_id), Value::from(alias)],
        )
        .await
        .expect("an alias reads");
    EntityId::new(
        rows.first()
            .unwrap_or_else(|| panic!("no entity {alias:?}"))
            .entity_id
            .clone(),
    )
}
