//! The fixture both test files open: rahi's single voter in a throwaway
//! directory on ports the OS allocated (spec 012 FR-001).
//!
//! `Store::open` with an empty `nodes` list is the chassis's single-voter
//! node (`rahi_store::StoreConfig::nodes`), which is what FR-001 means by
//! rahi's test harness at this layer: the crate under test is a library of
//! statements, so what it needs is a real hiqlite node, not a booted cell
//! binary. Acceptance criterion AC-2 exercises the binary's `migrate` and
//! `serve` verbs, which is the layer where those live.
//!
//! One node per test, because a node is a Raft group and two tests sharing
//! one would share its tables.

#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::net::{SocketAddr, TcpListener};
use std::path::Path;

use aicortex_types::{
    Actor, ActorId, Importance, Memory, MemoryBody, MemoryId, MemoryKind, MemoryParts, Provenance,
    Scope, SourceRef, SourceSystem, TrustClass,
};
use rahi_store::{EncKey, EncKeys, Envelope, Store, StoreConfig, StoreSecrets};
use rahi_types::{Revision, Sub, UnixSeconds};

/// A node in a temporary directory, stopped when the fixture drops.
pub struct Fixture {
    pub store: Store,
    pub dir: tempfile::TempDir,
}

impl Fixture {
    /// The handle every repository call takes.
    pub fn handle(&self) -> rahi_store::StoreHandle {
        self.store.handle()
    }

    /// The node with the crate's schema applied.
    pub async fn migrated() -> Self {
        let fixture = open().await;
        fixture
            .handle()
            .migrate(aicortex_store::migrations())
            .await
            .expect("the schema applies to an empty store");
        fixture
    }

    /// Stop the node.
    pub async fn shutdown(self) {
        self.store.shutdown().await.expect("the node stops");
    }
}

/// Open a single-voter node on free ports.
pub async fn open() -> Fixture {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let cfg = config(&dir.path().join("hiqlite"));
    let store = Store::open(&cfg).await.expect("a single-voter node opens");
    Fixture { store, dir }
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

/// A subject, as rauthy would have issued it.
pub fn sub(name: &str) -> Sub {
    Sub::new(format!("sub-{name}"))
}

/// A personal scope owned by `name`.
pub fn scope(name: &str) -> Scope {
    Scope::personal(sub(name))
}

/// A memory: the smallest one the record of 011 admits, in `scope`.
pub fn memory(scope: &Scope, text: &str, created: u64) -> Memory {
    memory_of_kind(scope, text, created, MemoryKind::Observation)
}

/// The same, of a chosen kind.
pub fn memory_of_kind(scope: &Scope, text: &str, created: u64, kind: MemoryKind) -> Memory {
    let created = UnixSeconds::new(created);
    Memory::new(MemoryParts {
        id: MemoryId::now_v7(),
        scope: scope.clone(),
        kind,
        body: MemoryBody::text(text),
        actor: Actor::human(ActorId::new("tester").expect("a legal actor id")),
        provenance: provenance(created),
        trust: TrustClass::Assertion,
        importance: Importance::at(created).expect("the default weight"),
        created,
    })
}

/// An original claim from a test source.
pub fn provenance(at: UnixSeconds) -> Provenance {
    Provenance::captured(
        SourceRef::new(SourceSystem::new("test").expect("a legal source system")),
        at,
        at,
    )
}

/// The outbox work a capture stages: the key of the memory that changed.
pub fn work(memory: &Memory) -> Envelope {
    Envelope::new(
        "memory",
        Some(memory.scope.owner.as_str().to_owned()),
        memory.id.to_string(),
        Revision::new(1),
    )
}
