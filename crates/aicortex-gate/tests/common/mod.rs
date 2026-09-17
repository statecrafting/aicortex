//! The committed corpus, read (spec 013 FR-001), and the node the end-to-end
//! capture of AC-2 runs against.
//!
//! A fixture is a JSON file under `testdata/corpus/`. It records what is
//! offered and what the gate must answer, and nothing about how: a reviewer
//! reads the file and can say whether the recorded verdict is the right one
//! without reading any Rust. That is what makes the corpus a review artefact
//! rather than a set of assertions in a test.

#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};

use aicortex_gate::{Candidate, Gate, Origin, RuleSet};
use aicortex_types::{
    Actor, ActorId, Importance, MediaDigest, MediaRef, MemoryBody, MemoryId, MemoryKind,
    MemoryParts, Provenance, Scope, SourceRef, SourceSystem, TrustClass,
};
use rahi_ledger::{Hash, Ledger, LedgerSigner};
use rahi_store::{EncKey, EncKeys, Store, StoreConfig, StoreSecrets};
use rahi_types::{Sub, UnixSeconds};
use serde::Deserialize;

/// One file of the corpus.
#[derive(Debug, Deserialize)]
pub struct Fixture {
    /// `verdict` for a case the gate judges, `benign-identifiers` for the
    /// FR-005 list.
    pub kind: String,
    /// The file's own name, repeated inside it so a failure names it.
    pub name: String,
    /// Why this case is in the corpus. Read by people, not by the test.
    pub why: String,
    /// How well the source is established (B-7). `authenticated` by default.
    #[serde(default)]
    pub origin: Option<String>,
    /// The source system the candidate claims.
    #[serde(default)]
    pub source_system: Option<String>,
    /// Source systems this fixture's deployment refuses (`PolicyDenied`).
    #[serde(default)]
    pub denied_sources: Vec<String>,
    /// What is offered.
    #[serde(default)]
    pub body: Option<FixtureBody>,
    /// The offending value, for FR-002's search.
    #[serde(default)]
    pub secret: Option<String>,
    /// What the gate must answer.
    #[serde(default)]
    pub expect: Option<Expect>,
    /// What normalization must leave behind (B-8), when the fixture pins it.
    #[serde(default)]
    pub normalized_text: Option<String>,
    /// The benign identifiers of FR-005.
    #[serde(default)]
    pub identifiers: Vec<String>,
}

/// A fixture's body, with two expansions so that a 64 KiB case is a line
/// rather than a 64 KiB file.
#[derive(Debug, Deserialize)]
pub struct FixtureBody {
    /// The text, or the unit that is repeated.
    pub text: String,
    /// How many times `text` is repeated. Absent means once.
    #[serde(default)]
    pub repeat: Option<usize>,
    /// The title, when the fixture has one.
    #[serde(default)]
    pub title: Option<String>,
    /// Media references to synthesize.
    #[serde(default)]
    pub media_repeat: Option<MediaRepeat>,
}

/// `times` media references of one type, each with its own digest.
#[derive(Debug, Deserialize)]
pub struct MediaRepeat {
    /// The IANA type every reference carries.
    pub media_type: String,
    /// How many.
    pub times: usize,
}

/// The recorded answer.
#[derive(Debug, Deserialize)]
pub struct Expect {
    /// `admit`, `quarantine` or `refuse`.
    pub verdict: String,
    /// The stable reason code, for the two that have one.
    #[serde(default)]
    pub code: Option<String>,
    /// The detector, for a `secret_detected`.
    #[serde(default)]
    pub detector: Option<String>,
    /// The media fault, for an `unsupported_media`.
    #[serde(default)]
    pub fault: Option<String>,
}

impl Fixture {
    /// The gate this fixture is judged by: the shipped rules, plus whatever
    /// denials the fixture's deployment declares.
    pub fn gate(&self) -> Gate {
        let rules = self
            .denied_sources
            .iter()
            .fold(RuleSet::standard(), |set, system| {
                set.denying(SourceSystem::new(system.clone()).expect("a legal source system"))
            });
        Gate::new(rules)
    }

    /// The candidate this fixture offers.
    pub fn candidate(&self) -> Candidate {
        let body = self
            .body
            .as_ref()
            .expect("a verdict fixture carries a body");
        let text = match body.repeat {
            Some(times) => body.text.repeat(times),
            None => body.text.clone(),
        };
        let mut memory_body = MemoryBody::text(text);
        if let Some(title) = &body.title {
            memory_body = memory_body.with_title(title.clone());
        }
        if let Some(media) = &body.media_repeat {
            for index in 0..media.times {
                memory_body = memory_body.with_media(
                    MediaRef::new(digest_of(index), media.media_type.clone())
                        .expect("a legal media type"),
                );
            }
        }
        let system = self.source_system.as_deref().unwrap_or("corpus");
        let at = UnixSeconds::new(1_800_000_000);
        Candidate::new(
            MemoryParts {
                id: MemoryId::now_v7(),
                scope: scope(&self.name),
                kind: MemoryKind::Observation,
                body: memory_body,
                actor: Actor::human(ActorId::new("corpus-reader").expect("a legal actor id")),
                provenance: Provenance::captured(
                    SourceRef::new(SourceSystem::new(system).expect("a legal source system")),
                    at,
                    at,
                ),
                trust: TrustClass::Assertion,
                importance: Importance::at(at).expect("the default weight"),
                created: at,
            },
            self.origin(),
        )
    }

    /// The origin this fixture declares.
    pub fn origin(&self) -> Origin {
        match self.origin.as_deref().unwrap_or("authenticated") {
            "authenticated" => Origin::Authenticated,
            "registered_adapter" => Origin::RegisteredAdapter,
            "asserted" => Origin::Asserted,
            "unestablished" => Origin::Unestablished,
            other => panic!("{}: {other} is not an origin", self.name),
        }
    }
}

/// A distinct, well-formed media digest per index.
fn digest_of(index: usize) -> MediaDigest {
    let hex = format!("{index:064x}");
    MediaDigest::parse(format!("sha256:{hex}")).expect("a legal digest")
}

/// Where the corpus lives.
pub fn corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/corpus")
}

/// Every fixture in the corpus, in a stable order.
pub fn corpus() -> Vec<Fixture> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(corpus_dir())
        .expect("the corpus directory is readable")
        .map(|entry| entry.expect("a directory entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    paths.sort();
    paths
        .iter()
        .map(|path| {
            let text = std::fs::read_to_string(path).expect("a corpus file");
            let fixture: Fixture = serde_json::from_str(&text)
                .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
            let stem = path.file_stem().expect("a file name").to_string_lossy();
            assert_eq!(
                fixture.name, stem,
                "a fixture's name and its file name disagree"
            );
            fixture
        })
        .collect()
}

/// A subject, as rauthy would have issued it.
pub fn sub(name: &str) -> Sub {
    Sub::new(format!("sub-{name}"))
}

/// A personal scope owned by `name`.
pub fn scope(name: &str) -> Scope {
    Scope::personal(sub(name))
}

/// A store and a ledger in a throwaway directory: the composition AC-2 is a
/// claim about.
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

    /// Stop the node.
    pub async fn shutdown(self) {
        self.store.shutdown().await.expect("the node stops");
    }
}

/// Open a single-voter node with the cell's schema and a fresh chain.
pub async fn node() -> Node {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let store = Store::open(&config(&dir.path().join("hiqlite")))
        .await
        .expect("a single-voter node opens");
    store
        .handle()
        .migrate(aicortex_store::migrations())
        .await
        .expect("the schema applies to an empty store");
    let ledger = Ledger::open(
        store.handle(),
        LedgerSigner::from_seed([7u8; 32]),
        Hash::parse(format!("sha256:{}", "ab".repeat(32))).expect("a legal manifest hash"),
    )
    .await
    .expect("a fresh chain opens");
    Node { store, ledger, dir }
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
            secret_raft: "raft-secret-for-aicortex-gate-tests".to_owned(),
            secret_api: "api-secret-for-aicortex-gate-tests".to_owned(),
            enc_keys: EncKeys {
                active: "test".to_owned(),
                keys: vec![EncKey {
                    id: "test".to_owned(),
                    key: vec![9u8; 32],
                }],
            },
        },
        backup_keep_days: 1,
        s3: None,
    }
}
