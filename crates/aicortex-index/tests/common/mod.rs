//! The store fixture the scan tests and the benchmark share (spec 016).
//!
//! A real single-voter rahi node in a throwaway directory, migrated with the
//! application schema, and a seeder that writes memory, chunk, and vector
//! rows directly so a test can hold tens of thousands of chunks without
//! running a provider.

#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::net::{SocketAddr, TcpListener};
use std::path::Path;

use aicortex_index::{Comparable, normalize};
use aicortex_store::ScopeId;
use aicortex_types::{MemoryId, Scope, Status};
use rahi_store::{
    EncKey, EncKeys, Statement, Store, StoreConfig, StoreHandle, StoreSecrets, Value,
};
use rahi_types::Sub;

/// The model identity every fixture vector is stored under.
pub const MODEL: &str = "fixture-model";

/// A node in a temporary directory.
pub struct Node {
    pub store: Store,
    pub dir: tempfile::TempDir,
}

impl Node {
    /// The handle every read takes.
    pub fn handle(&self) -> StoreHandle {
        self.store.handle()
    }

    /// A node with the application schema applied.
    pub async fn migrated() -> Self {
        let node = open().await;
        node.handle()
            .migrate(aicortex_store::migrations())
            .await
            .expect("the schema applies to an empty store");
        node
    }

    /// Stop the node.
    pub async fn shutdown(self) {
        self.store.shutdown().await.expect("the node stops");
    }
}

/// One fixture chunk.
#[derive(Clone, Debug)]
pub struct Chunk {
    pub memory: MemoryId,
    pub ordinal: u32,
    pub chunk_id: String,
    pub vector: Vec<f32>,
}

/// A deterministic memory id for `n`.
pub fn memory_id(n: u64) -> MemoryId {
    MemoryId::parse(&format!("00000000-0000-7000-8000-{n:012x}")).expect("a valid uuid")
}

/// The scope `name` and its id.
pub fn scope(name: &str) -> (Scope, ScopeId) {
    let scope = Scope::personal(Sub::new(name));
    let id = ScopeId::of(&scope);
    (scope, id)
}

/// A model revision, as spec 015 records one.
#[derive(Clone, Debug)]
pub struct ModelRevision {
    pub model_id: String,
    pub revision: u32,
    pub dims: u16,
    pub normalized: bool,
}

impl ModelRevision {
    /// The comparability key a scan matches rows on.
    pub fn comparable(&self) -> Comparable<'_> {
        Comparable {
            model_id: &self.model_id,
            revision: self.revision,
            dims: self.dims,
            normalized: self.normalized,
        }
    }
}

/// A model revision at `revision` with `dims`.
pub fn revision(revision: u32, dims: u16) -> ModelRevision {
    ModelRevision {
        model_id: MODEL.to_owned(),
        revision,
        dims,
        normalized: true,
    }
}

/// A fixed pseudo-random unit vector for `seed`.
pub fn unit_vector(seed: u64, dims: u16) -> Vec<f32> {
    let mut state = seed
        .wrapping_mul(0x9e37_79b9_7f4a_7c15)
        .wrapping_add(0x2545_f491_4f6c_dd1d);
    let mut values: Vec<f32> = (0..dims)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let unit = u32::try_from(state >> 40).unwrap();
            (f64::from(unit) / f64::from(1_u32 << 24)).mul_add(2.0, -1.0) as f32
        })
        .collect();
    normalize(&mut values).expect("a random vector has a direction");
    values
}

/// Register a model row at `model`'s revision, whatever is already active.
pub async fn register_model(store: &StoreHandle, model: &ModelRevision) {
    store
        .execute(
            "INSERT INTO embedding_model (model_id, revision, dims, normalized, first_seen, active)
             VALUES ($1, $2, $3, $4, 1700000000, 0)",
            vec![
                Value::from(model.model_id.as_str()),
                Value::Integer(i64::from(model.revision)),
                Value::Integer(i64::from(model.dims)),
                Value::from(model.normalized),
            ],
        )
        .await
        .expect("the model row is written");
}

/// Write the scope row for `scope`.
pub async fn register_scope(store: &StoreHandle, scope: &Scope) {
    store
        .execute(
            "INSERT INTO scope (scope_id, owner, kind, key, created)
             VALUES ($1, $2, $3, $4, 1700000000)",
            vec![
                Value::from(ScopeId::of(scope).as_str()),
                Value::from(scope.owner.as_str()),
                Value::from(scope.kind.label()),
                Value::from(ScopeId::key_of(scope)),
            ],
        )
        .await
        .expect("the scope row is written");
}

/// `memories * per_memory` chunks of `dims` in `scope_id`, deterministic in
/// `seed`. Memory ids start at `first_memory`.
pub fn chunks(
    scope_id: &ScopeId,
    model: &ModelRevision,
    first_memory: u64,
    memories: u64,
    per_memory: u32,
    seed: u64,
) -> Vec<Chunk> {
    let mut out = Vec::new();
    for m in first_memory..first_memory + memories {
        for ordinal in 0..per_memory {
            let memory = memory_id(m);
            out.push(Chunk {
                memory,
                ordinal,
                chunk_id: chunk_id(scope_id, memory, model.revision, ordinal),
                vector: unit_vector(
                    seed ^ (m << 8) ^ u64::from(ordinal) ^ (u64::from(model.revision) << 48),
                    model.dims,
                ),
            });
        }
    }
    out
}

/// A chunk id unique across scopes and revisions.
pub fn chunk_id(scope_id: &ScopeId, memory: MemoryId, revision: u32, ordinal: u32) -> String {
    let prefix = scope_id.as_str().get(..12).unwrap_or(scope_id.as_str());
    format!("{prefix}-{memory}-{revision}-{ordinal:04}")
}

/// Write `chunks` with their memories (at `status`) under `model`.
///
/// Memories already present are left as they are, so a second revision's
/// chunks can be written for the same memories.
pub async fn seed(
    store: &StoreHandle,
    scope_id: &ScopeId,
    model: &ModelRevision,
    chunks: &[Chunk],
    status: Status,
) {
    // Keep each transaction well under the engine's write-ahead ceiling.
    const BUDGET: usize = 512 * 1024;
    let mut batch = Vec::new();
    let mut bytes = 0;
    let mut last_memory = None;
    for chunk in chunks {
        if last_memory != Some(chunk.memory) {
            batch.push(memory_row(scope_id, chunk.memory, status));
            last_memory = Some(chunk.memory);
        }
        batch.push(Statement::with_params(
            "INSERT INTO chunk
             (chunk_id, scope_id, memory_id, model_revision, ordinal, byte_start, byte_end)
             VALUES ($1, $2, $3, $4, $5, 0, 1)",
            vec![
                Value::from(chunk.chunk_id.as_str()),
                Value::from(scope_id.as_str()),
                Value::from(chunk.memory.to_string()),
                Value::Integer(i64::from(model.revision)),
                Value::Integer(i64::from(chunk.ordinal)),
            ],
        ));
        let blob: Vec<u8> = chunk.vector.iter().flat_map(|v| v.to_le_bytes()).collect();
        bytes += blob.len() + 256;
        batch.push(Statement::with_params(
            "INSERT INTO embedding
             (scope_id, memory_id, model_id, model_revision, chunk_ordinal, dims,
              normalized, vector, updated)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 1700000000)",
            vec![
                Value::from(scope_id.as_str()),
                Value::from(chunk.memory.to_string()),
                Value::from(model.model_id.as_str()),
                Value::Integer(i64::from(model.revision)),
                Value::Integer(i64::from(chunk.ordinal)),
                Value::Integer(i64::from(model.dims)),
                Value::from(model.normalized),
                Value::Blob(blob),
            ],
        ));
        if bytes >= BUDGET {
            store
                .txn(std::mem::take(&mut batch))
                .await
                .expect("a seed batch commits");
            bytes = 0;
        }
    }
    if !batch.is_empty() {
        store.txn(batch).await.expect("the last seed batch commits");
    }
}

fn memory_row(scope_id: &ScopeId, memory: MemoryId, status: Status) -> Statement {
    Statement::with_params(
        "INSERT INTO memory
         (id, scope_id, status, kind, trust, fingerprint, schema_version, body_bytes,
          created, updated, record)
         VALUES ($1, $2, $3, 'note', 'first_party', $1, 1, 1, 1700000000, 1700000000, '{}')
         ON CONFLICT (id) DO NOTHING",
        vec![
            Value::from(memory.to_string()),
            Value::from(scope_id.as_str()),
            Value::from(status.label()),
        ],
    )
}

/// Open a single-voter node on free ports, retrying a port collision.
pub async fn open() -> Node {
    let mut last = None;
    for _ in 0..5 {
        let dir = tempfile::tempdir().expect("a temporary directory");
        match Store::open(&config(&dir.path().join("hiqlite"))).await {
            Ok(store) => return Node { store, dir },
            Err(error) if error.message().contains("Address already in use") => {
                last = Some(error);
            }
            Err(error) => panic!("a single-voter node opens: {error}"),
        }
    }
    panic!(
        "a single-voter node opens after bounded port retries: {}",
        last.expect("a port collision was recorded")
    )
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
            secret_raft: "raft-secret-for-aicortex-tests".to_owned(),
            secret_api: "api-secret-for-aicortex-tests".to_owned(),
            enc_keys: EncKeys {
                active: "test".to_owned(),
                keys: vec![EncKey {
                    id: "test".to_owned(),
                    key: vec![12u8; 32],
                }],
            },
        },
        backup_keep_days: 1,
        s3: None,
    }
}
