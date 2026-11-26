//! The store fixture the recall tests share (spec 018).
//!
//! A real single-voter rahi node migrated with the application schema.
//! Memories, provenance, chunks, vectors, and text postings are written as
//! rows, so each test controls exactly what every channel can see, and a
//! two-wide keyword embedder stands in for a model.

#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::net::{SocketAddr, TcpListener};
use std::path::Path;

use aicortex_embed::{EmbeddingProvider, ModelId, Vector};
use aicortex_index::text::{ChunkText, stage_chunk};
use aicortex_index::tokenize::TokenizerConfig;
use aicortex_recall::{FixedClock, Recall, RecallConfig};
use aicortex_store::ScopeId;
use aicortex_types::{
    Actor, ActorId, Importance, Memory, MemoryBody, MemoryId, MemoryKind, MemoryParts, Provenance,
    Scope, SourceRef, SourceSystem, TrustClass,
};
use rahi_store::{
    EncKey, EncKeys, Statement, Store, StoreConfig, StoreHandle, StoreSecrets, TxnBuilder, Value,
};
use rahi_types::{Error, Sub, UnixSeconds};

/// The fixture clock: ten days after the epoch of the fixture's times.
pub const NOW: u64 = 1_700_000_000 + 10 * DAY;
pub const DAY: u64 = 86_400;
pub const MODEL: &str = "fixture-keyword";

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

/// A migrated node with the fixture model registered and active.
pub async fn migrated() -> Node {
    for _ in 0..5 {
        let dir = tempfile::tempdir().expect("a temporary directory");
        match Store::open(&config(&dir.path().join("hiqlite"))).await {
            Ok(store) => {
                let handle = store.handle();
                handle
                    .migrate(aicortex_store::migrations())
                    .await
                    .expect("the schema applies");
                handle
                    .execute(
                        "INSERT INTO embedding_model
                         (model_id, revision, dims, normalized, first_seen, active)
                         VALUES (?1, 1, 2, 1, 1700000000, 1)",
                        vec![Value::from(MODEL)],
                    )
                    .await
                    .expect("the model row");
                return Node { store, dir };
            }
            Err(error) if error.message().contains("Address already in use") => {}
            Err(error) => panic!("a single-voter node opens: {error}"),
        }
    }
    panic!("a node opens after bounded port retries");
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
            secret_raft: "raft-secret-for-recall-tests".to_owned(),
            secret_api: "api-secret-for-recall-tests".to_owned(),
            enc_keys: EncKeys {
                active: "test".to_owned(),
                keys: vec![EncKey {
                    id: "test".to_owned(),
                    key: vec![31_u8; 32],
                }],
            },
        },
        backup_keep_days: 1,
        s3: None,
    }
}

/// The keyword embedder: `apple` points one way, `boat` the other.
#[derive(Clone, Copy, Debug)]
pub struct Keyword;

pub fn embed_text(text: &str) -> [f32; 2] {
    let text = text.to_lowercase();
    match (text.contains("apple"), text.contains("boat")) {
        (true, false) => [1.0, 0.0],
        (false, true) => [0.0, 1.0],
        _ => [
            std::f32::consts::FRAC_1_SQRT_2,
            std::f32::consts::FRAC_1_SQRT_2,
        ],
    }
}

impl EmbeddingProvider for Keyword {
    fn id(&self) -> ModelId {
        ModelId::new(MODEL).expect("a model id")
    }

    fn dims(&self) -> u16 {
        2
    }

    fn normalized(&self) -> bool {
        true
    }

    async fn embed(&self, batch: &[&str]) -> Result<Vec<Vector>, Error> {
        batch
            .iter()
            .map(|text| Vector::new(embed_text(text).to_vec()))
            .collect()
    }
}

pub fn recall(node: &Node) -> Recall<Keyword, FixedClock> {
    Recall::new(
        node.handle(),
        Keyword,
        FixedClock(UnixSeconds::new(NOW)),
        RecallConfig::default(),
    )
}

/// A deterministic memory id.
pub fn memory_id(n: u64) -> MemoryId {
    MemoryId::parse(&format!("00000000-0000-7000-8000-{n:012x}")).expect("a valid uuid")
}

pub fn scope(name: &str) -> (Scope, ScopeId) {
    let scope = Scope::personal(Sub::new(name));
    let id = ScopeId::of(&scope);
    (scope, id)
}

/// One fixture memory.
#[derive(Clone, Debug)]
pub struct Doc {
    pub n: u64,
    pub text: String,
    pub age_days: u64,
    pub status: &'static str,
    pub kind: MemoryKind,
    pub source: &'static str,
    pub importance: f32,
}

impl Doc {
    pub fn new(n: u64, text: &str, age_days: u64) -> Self {
        Self {
            n,
            text: text.to_owned(),
            age_days,
            status: "active",
            kind: MemoryKind::Observation,
            source: "notes",
            importance: 0.5,
        }
    }

    pub fn status(mut self, status: &'static str) -> Self {
        self.status = status;
        self
    }

    pub fn source(mut self, source: &'static str) -> Self {
        self.source = source;
        self
    }

    pub fn importance(mut self, importance: f32) -> Self {
        self.importance = importance;
        self
    }

    pub fn created(&self) -> u64 {
        NOW - self.age_days * DAY
    }
}

fn memory_of(scope: &Scope, doc: &Doc) -> Memory {
    let at = UnixSeconds::new(doc.created());
    Memory::new(MemoryParts {
        id: memory_id(doc.n),
        scope: scope.clone(),
        kind: doc.kind,
        body: MemoryBody::text(&doc.text),
        actor: Actor::human(ActorId::new("tester").expect("actor id")),
        provenance: Provenance::captured(
            SourceRef::new(SourceSystem::new(doc.source).expect("source system")),
            at,
            at,
        ),
        trust: TrustClass::Assertion,
        importance: Importance::new(doc.importance, UnixSeconds::new(NOW), 1).expect("importance"),
        created: at,
    })
}

/// Write `doc` with its provenance, chunk, vector, and postings.
pub async fn add(store: &StoreHandle, scope: &Scope, doc: &Doc) -> MemoryId {
    let scope_id = ScopeId::of(scope);
    let memory = memory_of(scope, doc);
    let record = serde_json::to_string(&memory).expect("a record");
    let created = i64::try_from(doc.created()).unwrap();
    let id = memory.id.to_string();
    let chunk_id = format!("chunk-{}-{id}", &scope_id.as_str()[..8]);
    let vector: Vec<u8> = embed_text(&doc.text)
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect();
    let mut txn = TxnBuilder::new();
    txn.push(Statement::with_params(
        "INSERT INTO scope (scope_id, owner, kind, key, created)
         VALUES (?1, ?2, ?3, ?4, 1700000000) ON CONFLICT DO NOTHING",
        vec![
            Value::from(scope_id.as_str()),
            Value::from(scope.owner.as_str()),
            Value::from(scope.kind.label()),
            Value::from(ScopeId::key_of(scope)),
        ],
    ));
    txn.push(Statement::with_params(
        "INSERT INTO memory
         (id, scope_id, status, kind, trust, fingerprint, schema_version, body_bytes,
          created, updated, record)
         VALUES (?1, ?2, ?3, ?4, 'assertion', ?1, 1, 1, ?5, ?5, ?6)",
        vec![
            Value::from(id.as_str()),
            Value::from(scope_id.as_str()),
            Value::from(doc.status),
            Value::from(doc.kind.label()),
            Value::Integer(created),
            Value::from(record),
        ],
    ));
    txn.push(Statement::with_params(
        "INSERT INTO provenance
         (memory_id, scope_id, source_system, source_external_id, source_locator,
          captured_at, ingested_at)
         VALUES (?1, ?2, ?3, NULL, NULL, ?4, ?4)",
        vec![
            Value::from(id.as_str()),
            Value::from(scope_id.as_str()),
            Value::from(doc.source),
            Value::Integer(created),
        ],
    ));
    txn.push(Statement::with_params(
        "INSERT INTO chunk
         (chunk_id, scope_id, memory_id, model_revision, ordinal, byte_start, byte_end)
         VALUES (?1, ?2, ?3, 1, 0, 0, ?4)",
        vec![
            Value::from(chunk_id.as_str()),
            Value::from(scope_id.as_str()),
            Value::from(id.as_str()),
            Value::Integer(i64::try_from(doc.text.len()).unwrap()),
        ],
    ));
    txn.push(Statement::with_params(
        "INSERT INTO embedding
         (scope_id, memory_id, model_id, model_revision, chunk_ordinal, dims,
          normalized, vector, updated)
         VALUES (?1, ?2, ?3, 1, 0, 2, 1, ?4, 1700000000)",
        vec![
            Value::from(scope_id.as_str()),
            Value::from(id.as_str()),
            Value::from(MODEL),
            Value::Blob(vector),
        ],
    ));
    stage_chunk(
        &mut txn,
        &ChunkText {
            scope_id: scope_id.as_str(),
            memory_id: memory.id,
            revision: 1,
            chunk_id: &chunk_id,
            text: &doc.text,
        },
        &TokenizerConfig::default(),
    );
    store
        .txn(txn.into_statements())
        .await
        .expect("the memory commits");
    memory.id
}

/// Write an entity named `name` (id `entity`) and a mention of it in each of
/// `memories`.
pub async fn add_entity(
    store: &StoreHandle,
    scope_id: &ScopeId,
    entity: &str,
    name: &str,
    memories: &[MemoryId],
) {
    let mut batch = vec![
        Statement::with_params(
            "INSERT INTO entity (entity_id, scope_id, kind, canonical, first_seen, last_seen)
             VALUES (?1, ?2, 'person', ?3, 1700000000, 1700000000)",
            vec![
                Value::from(entity),
                Value::from(scope_id.as_str()),
                Value::from(name),
            ],
        ),
        Statement::with_params(
            "INSERT INTO entity_alias (scope_id, entity_id, alias, normalized)
             VALUES (?1, ?2, ?3, ?4)",
            vec![
                Value::from(scope_id.as_str()),
                Value::from(entity),
                Value::from(name),
                Value::from(aicortex_graph::normalize(name)),
            ],
        ),
    ];
    for memory in memories {
        batch.push(Statement::with_params(
            "INSERT INTO mention
             (scope_id, memory_id, entity_id, byte_start, byte_end, confidence,
              extractor, extractor_version)
             VALUES (?1, ?2, ?3, 0, 4, 900, 'fixture', 1)",
            vec![
                Value::from(scope_id.as_str()),
                Value::from(memory.to_string()),
                Value::from(entity),
            ],
        ));
    }
    store.txn(batch).await.expect("the entity commits");
}

/// Set a memory's status column, as erasure or quarantine would.
pub async fn set_status(store: &StoreHandle, scope_id: &ScopeId, memory: MemoryId, status: &str) {
    store
        .execute(
            "UPDATE memory SET status = ?3 WHERE scope_id = ?1 AND id = ?2",
            vec![
                Value::from(scope_id.as_str()),
                Value::from(memory.to_string()),
                Value::from(status),
            ],
        )
        .await
        .expect("the status updates");
}

/// The fixture corpus every ordering test shares. Ids 1 to 5.
pub async fn corpus(store: &StoreHandle, scope: &Scope) {
    for doc in [
        Doc::new(1, "apple pie recipe with cinnamon", 30),
        Doc::new(2, "visit to the apple orchard", 1),
        Doc::new(3, "boat trip across the lake", 2),
        Doc::new(4, "apple and boat festival", 3),
        Doc::new(5, "quiet note about taxes", 60).source("mail"),
    ] {
        add(store, scope, &doc).await;
    }
}

/// The fixture number a memory id was minted from.
pub fn n_of(id: MemoryId) -> u64 {
    let text = id.to_string();
    u64::from_str_radix(&text[text.len() - 12..], 16).expect("a fixture id")
}

/// Fixture numbers, in order.
pub fn numbers(result: &aicortex_recall::RecallResult) -> Vec<u64> {
    result.memories.iter().map(|m| n_of(m.memory_id)).collect()
}
