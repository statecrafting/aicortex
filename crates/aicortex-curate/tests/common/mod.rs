//! The fixture the review tests share (spec 023).
//!
//! A real single-voter rahi node in a throwaway directory, migrated with the
//! application schema, and the chassis's real decision chain on top of it
//! (AC-2 asserts that a real chain still verifies after the decisions). Rows
//! are written directly so a test controls exactly what the queue reads.

#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::net::{SocketAddr, TcpListener};
use std::path::Path;

use aicortex_store::{Counters, ScopeId};
use aicortex_types::{
    Actor, ActorId, Importance, Memory, MemoryBody, MemoryId, MemoryKind, MemoryParts, Provenance,
    Scope, SourceRef, SourceSystem, Status, TrustClass,
};
use rahi_ledger::{Hash, Ledger, LedgerSigner};
use rahi_store::{
    EncKey, EncKeys, Statement, Store, StoreConfig, StoreHandle, StoreSecrets, TxnBuilder, Value,
};
use rahi_types::{Sub, UnixSeconds};

/// A node and its chain, stopped when the fixture is shut down.
pub struct Node {
    pub store: Store,
    pub ledger: Ledger,
    pub dir: tempfile::TempDir,
}

impl Node {
    pub fn handle(&self) -> StoreHandle {
        self.store.handle()
    }

    pub async fn shutdown(self) {
        self.store.shutdown().await.expect("the node stops");
    }

    /// How many Decisions of `kind` the chain holds.
    pub async fn decisions(&self, kind: &str) -> usize {
        let mut count = 0;
        for record in self.ledger.records().await.expect("the chain reads") {
            if record.decision().expect("a record decodes").kind.as_str() == kind {
                count += 1;
            }
        }
        count
    }
}

/// A migrated node with a chain at genesis.
pub async fn node() -> Node {
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
                let ledger = Ledger::open(
                    store.handle(),
                    LedgerSigner::from_seed([9_u8; 32]),
                    Hash::parse(format!("sha256:{}", "cd".repeat(32))).expect("a manifest hash"),
                )
                .await
                .expect("a fresh chain opens");
                return Node { store, ledger, dir };
            }
            Err(error) if error.message().contains("Address already in use") => last = Some(error),
            Err(error) => panic!("a single-voter node opens: {error}"),
        }
    }
    panic!("a node opens after bounded port retries: {last:?}");
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
            secret_raft: "raft-secret-for-review-tests".to_owned(),
            secret_api: "api-secret-for-review-tests".to_owned(),
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

/// The scope owned by `name`.
pub fn scope(name: &str) -> Scope {
    Scope::personal(Sub::new(name))
}

/// A person's subject.
pub fn person(name: &str) -> Sub {
    Sub::new(name)
}

/// Write a memory row in `status` with `text`, its scope row, and its counter.
pub async fn add_memory(
    store: &StoreHandle,
    scope: &Scope,
    text: &str,
    status: Status,
    trust: TrustClass,
    created: u64,
) -> MemoryId {
    let at = UnixSeconds::new(created);
    let mut memory = Memory::new(MemoryParts {
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
        trust,
        importance: Importance::at(at).expect("importance"),
        created: at,
    });
    memory.status = status;
    let scope_id = ScopeId::of(scope);
    let mut txn = TxnBuilder::new();
    txn.push(Statement::with_params(
        "INSERT INTO scope (scope_id, owner, kind, key, created)
         VALUES (?1, ?2, ?3, ?4, 1700000000) ON CONFLICT DO NOTHING",
        vec![
            Value::from(&scope_id),
            Value::from(scope.owner.as_str()),
            Value::from(scope.kind.label()),
            Value::from(ScopeId::key_of(scope)),
        ],
    ));
    txn.push(Statement::with_params(
        "INSERT INTO memory
         (id, scope_id, status, kind, trust, fingerprint, schema_version, body_bytes,
          created, updated, record)
         VALUES (?1, ?2, ?3, ?4, ?5, ?1, 1, 1, ?6, ?6, ?7)",
        vec![
            Value::from(memory.id.to_string()),
            Value::from(&scope_id),
            Value::from(memory.status.label()),
            Value::from(memory.kind.label()),
            Value::from(memory.trust.label()),
            Value::Integer(i64::try_from(created).unwrap()),
            Value::from(serde_json::to_string(&memory).unwrap()),
        ],
    ));
    Counters::increment(&mut txn, &scope_id, memory.kind, &memory.status);
    store
        .txn(txn.into_statements())
        .await
        .expect("the memory row commits");
    memory.id
}

/// Write an active edge `subject --works_on--> literal` valid from `from` to
/// `until`, sourced from `memory`.
pub async fn add_edge(
    store: &StoreHandle,
    scope: &Scope,
    id: &str,
    literal: &str,
    from: i64,
    until: Option<i64>,
    memory: MemoryId,
) {
    let scope_id = ScopeId::of(scope);
    store
        .txn(vec![
            Statement::with_params(
                "INSERT INTO edge
                 (edge_id, scope_id, subject, predicate, object_literal, confidence, extractor,
                  extractor_version, valid_from, valid_until, status, created)
                 VALUES (?1, ?2, 'ent-alice', 'works_on', ?3, 900, 'test', 1, ?4, ?5, 'active', ?4)",
                vec![
                    Value::from(id),
                    Value::from(&scope_id),
                    Value::from(literal),
                    Value::Integer(from),
                    Value::from(until),
                ],
            ),
            Statement::with_params(
                "INSERT INTO edge_source (scope_id, edge_id, memory_id) VALUES (?1, ?2, ?3)",
                vec![
                    Value::from(&scope_id),
                    Value::from(id),
                    Value::from(memory.to_string()),
                ],
            ),
        ])
        .await
        .expect("the edge commits");
}

/// Write the open contradiction between two edges and return its id.
pub async fn add_contradiction(store: &StoreHandle, scope: &Scope, a: &str, b: &str) -> String {
    let id = format!("ctd-{a}-{b}");
    store
        .txn(vec![Statement::with_params(
            "INSERT INTO contradiction
               (contradiction_id, scope_id, edge_a, edge_b, subject, predicate, detected)
             VALUES (?1, ?2, ?3, ?4, 'ent-alice', 'works_on', 1700000100)",
            vec![
                Value::from(id.as_str()),
                Value::from(&ScopeId::of(scope)),
                Value::from(a),
                Value::from(b),
            ],
        )])
        .await
        .expect("the contradiction commits");
    id
}

#[derive(serde::Deserialize)]
pub struct EdgeState {
    pub status: String,
    pub valid_from: i64,
    pub valid_until: Option<i64>,
}

/// One edge's standing.
pub async fn edge(store: &StoreHandle, scope: &Scope, id: &str) -> EdgeState {
    let rows: Vec<EdgeState> = store
        .query_consistent(
            "SELECT status, valid_from, valid_until FROM edge WHERE scope_id = ?1 AND edge_id = ?2",
            vec![Value::from(&ScopeId::of(scope)), Value::from(id)],
        )
        .await
        .expect("the edge reads");
    rows.into_iter().next().expect("the edge exists")
}

#[derive(serde::Deserialize)]
struct Count {
    count: i64,
}

/// How many rows `sql` counts, with `scope` bound as `?1`.
pub async fn count(store: &StoreHandle, scope: &Scope, sql: &'static str) -> i64 {
    let rows: Vec<Count> = store
        .query_consistent(sql, vec![Value::from(&ScopeId::of(scope))])
        .await
        .expect("the count reads");
    rows.into_iter().next().expect("a count row").count
}
