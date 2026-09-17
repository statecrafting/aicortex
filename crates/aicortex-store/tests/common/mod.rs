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

use aicortex_gate::{Admitted, Candidate, Gate, Origin, Verdict};
use aicortex_types::{
    Actor, ActorId, Importance, Memory, MemoryBody, MemoryId, MemoryKind, MemoryParts, Provenance,
    Scope, SourceRef, SourceSystem, TrustClass,
};
use rahi_ledger::{Hash, Ledger, LedgerSigner};
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

/// A node with the crate's schema and a fresh decision chain (spec 014).
///
/// The erasure of 014 B-7 appends a Decision, so its tests need a chain as
/// well as a store. The chain is rahi's, opened exactly as the cell opens it:
/// there is no second ledger here and no test double for one, because what
/// AC-2 asserts is that a real chain still verifies after a real erasure.
pub struct Node {
    pub store: Store,
    pub ledger: Ledger,
    pub dir: tempfile::TempDir,
}

impl Node {
    /// The handle every repository call takes.
    pub fn handle(&self) -> rahi_store::StoreHandle {
        self.store.handle()
    }

    /// Close every handle and reopen the same durable node, with its persisted Raft addresses.
    pub async fn reopen(self) -> Self {
        let Self { store, ledger, dir } = self;
        let cfg = store.config().clone();
        drop(ledger);
        store
            .shutdown()
            .await
            .expect("the node stops before reopen");
        drop(store);
        let store = tokio::time::timeout(std::time::Duration::from_secs(30), Store::open(&cfg))
            .await
            .expect("reopen finishes within 30 seconds")
            .expect("the durable node reopens");
        let ledger = Ledger::open(
            store.handle(),
            LedgerSigner::from_seed([7u8; 32]),
            Hash::parse(format!("sha256:{}", "ab".repeat(32))).unwrap(),
        )
        .await
        .expect("the durable chain reopens");
        Self { store, ledger, dir }
    }

    /// Restore committed app data into a fresh node. The cached lease state
    /// is outside this recovery boundary: a full-node scope restart can wait
    /// indefinitely while reacquiring a lease in the pinned chassis.
    /// This proves durable SQL recovery, not lease restart or TTL safety.
    pub async fn recover_app_snapshot(self) -> Self {
        let bytes = backup_bytes(&self).await;
        self.shutdown().await;
        recover_snapshot(&bytes).await
    }

    /// Stop the node.
    pub async fn shutdown(self) {
        self.store.shutdown().await.expect("the node stops");
    }
}

/// Open a single-voter node with the schema applied and a chain at genesis.
pub async fn node() -> Node {
    let fixture = Fixture::migrated().await;
    let ledger = Ledger::open(
        fixture.handle(),
        LedgerSigner::from_seed([7u8; 32]),
        Hash::parse(format!("sha256:{}", "ab".repeat(32))).expect("a legal manifest hash"),
    )
    .await
    .expect("a fresh chain opens");
    Node {
        store: fixture.store,
        ledger,
        dir: fixture.dir,
    }
}

/// Boot an isolated store from the app database snapshot, using the same
/// on-disk placement as rahi-ops 0.1.0's restore. This tests database recovery,
/// not the operator archive, identity-store restore, or production deployment.
pub async fn recover_snapshot(bytes: &[u8]) -> Node {
    let dir = tempfile::tempdir().expect("a recovery directory");
    let data_dir = dir.path().join("hiqlite");
    let db_dir = data_dir.join("state_machine/db");
    std::fs::create_dir_all(&db_dir).expect("the database directory");
    std::fs::write(db_dir.join("hiqlite.db"), bytes).expect("the snapshot is placed");
    let store = Store::open(&config(&data_dir))
        .await
        .expect("the snapshot boots");
    let ledger = Ledger::open(
        store.handle(),
        LedgerSigner::from_seed([7u8; 32]),
        Hash::parse(format!("sha256:{}", "ab".repeat(32))).unwrap(),
    )
    .await
    .expect("the recovered chain opens");
    Node { store, ledger, dir }
}

/// How many rows a statement counts, for a test asserting about the tables
/// directly rather than through a repository.
pub async fn count(node: &Node, sql: &str, params: Vec<rahi_store::Value>) -> u64 {
    #[derive(serde::Deserialize)]
    struct Count {
        count: i64,
    }
    let rows: Vec<Count> = node
        .handle()
        .query_consistent(sql.to_owned(), params)
        .await
        .expect("a count reads");
    u64::try_from(rows.first().map_or(0, |row| row.count)).expect("a non-negative count")
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

/// The same, naming the memories it was derived from (spec 014 B-4, B-8).
pub fn derived_memory(
    scope: &Scope,
    text: &str,
    created: u64,
    kind: MemoryKind,
    parents: Vec<MemoryId>,
) -> Memory {
    let mut memory = memory_of_kind(scope, text, created, kind);
    let at = UnixSeconds::new(created);
    memory.provenance = Provenance::captured(
        SourceRef::new(SourceSystem::new("test").expect("a legal source system")),
        at,
        at,
    )
    .derived(
        parents,
        aicortex_types::ExtractorVersion {
            name: "test-extractor".to_owned(),
            version: "1".to_owned(),
        },
    );
    memory
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

/// The verdict the shipped gate returns for `memory`, offered over an
/// authenticated channel (spec 013 B-1, B-7).
///
/// `MemoryRepo::insert` takes an `Admitted`, and the only way to one is a
/// verdict, so every capture in this crate's tests goes through the real gate
/// rather than around it. That is the point of B-1: there is no test-only
/// door, because a test-only door is a door.
pub fn verdict(memory: &Memory) -> Verdict {
    Gate::standard().evaluate(&Candidate::new(parts_of(memory), Origin::Authenticated))
}

/// `memory`, admitted by the real gate.
///
/// Panics when the gate does not admit it, and when the gate changed it:
/// these fixtures are plain ASCII with no trailing whitespace, so
/// normalization is the identity on them and every assertion the tests make
/// about the memory they built still holds of the memory that is stored.
pub fn admit(memory: &Memory) -> Admitted {
    match verdict(memory) {
        Verdict::Admit(admitted) => {
            assert_eq!(
                admitted.memory(),
                memory,
                "the gate normalized a fixture, so the test is asserting about a different memory"
            );
            admitted
        }
        other => panic!("the gate did not admit a fixture: {other:?}"),
    }
}

/// A memory taken apart into the parts a candidate is offered as.
pub fn parts_of(memory: &Memory) -> MemoryParts {
    MemoryParts {
        id: memory.id,
        scope: memory.scope.clone(),
        kind: memory.kind,
        body: memory.body.clone(),
        actor: memory.actor.clone(),
        provenance: memory.provenance.clone(),
        trust: memory.trust.clone(),
        importance: memory.importance,
        created: memory.created,
    }
}

/// Take a backup and read back every byte of the file it wrote.
///
/// The trigger is the chassis's `backup`, and the file is then found on disk
/// rather than trusted to be listed by the time that call returns: hiqlite
/// writes the snapshot on a task of its own, so the listing rahi compares
/// against can legitimately race it and answer "no new file". Whether that
/// call reports the name or not, the snapshot is the one that lands in the
/// backup directory, and waiting for its size to settle is what makes reading
/// it deterministic.
pub async fn backup_bytes(node: &Node) -> Vec<u8> {
    let before = backup_files(node);
    let reported = node.handle().backup().await;
    for _ in 0..200_u32 {
        let fresh: Vec<std::path::PathBuf> = backup_files(node)
            .into_iter()
            .filter(|path| !before.contains(path))
            .collect();
        if let Some(path) = fresh.first() {
            return settled(path).await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("no backup file appeared; the trigger reported {reported:?}");
}

/// The snapshots hiqlite has written under this node's data directory.
fn backup_files(node: &Node) -> Vec<std::path::PathBuf> {
    let mut files: Vec<std::path::PathBuf> = walkdir(node.dir.path())
        .into_iter()
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("backup_"))
        })
        .collect();
    files.sort();
    files
}

/// Read a file once its size has stopped changing.
async fn settled(path: &std::path::Path) -> Vec<u8> {
    let mut last = 0_u64;
    for _ in 0..200_u32 {
        let size = std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0);
        if size > 0 && size == last {
            return std::fs::read(path).expect("the backup file reads");
        }
        last = size;
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("the backup file {} never stopped growing", path.display());
}

/// Every file under `root`, depth first.
fn walkdir(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                out.push(path);
            }
        }
    }
    out
}
