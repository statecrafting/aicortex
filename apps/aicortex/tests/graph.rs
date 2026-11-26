//! The graph at the application boundary (spec 017 AC-2, B-7).
//!
//! These tests run the seams the chassis calls against the cell's real
//! migrations: the `app.graph` preflight check, and the extraction service
//! that turns a live memory into entities and edges and returns on shutdown.

#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used
)]

use std::net::{SocketAddr, TcpListener};
use std::time::{Duration, Instant};

use aicortex::Aicortex;
use aicortex::graph_preflight;
use aicortex::graph_service;
use aicortex_graph::{ExtractionConfig, ExtractionWorker, PatternExtractor, read_scope};
use aicortex_types::{
    Actor, ActorId, Importance, Memory, MemoryBody, MemoryId, MemoryKind, MemoryParts, Provenance,
    Scope, SourceRef, SourceSystem, TrustClass,
};
use rahi_cli::{AppOutcome, Cell};
use rahi_store::{
    EncKey, EncKeys, FailureDetail, RetryPolicy, Store, StoreConfig, StoreHandle, StoreSecrets,
    TxnBuilder, Value, Work,
};
use rahi_types::{Sub, UnixSeconds};
use tokio::sync::watch;

const SCOPE: &str = "scope-a";

fn free_addr() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("free loopback port");
    listener.local_addr().expect("bound address")
}

fn store_config(data_dir: std::path::PathBuf) -> StoreConfig {
    StoreConfig {
        node_id: 1,
        nodes: Vec::new(),
        data_dir,
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
    }
}

struct Fixture {
    store: Store,
    _directory: tempfile::TempDir,
}

impl Fixture {
    async fn migrated() -> Self {
        let mut last = None;
        for _ in 0..5 {
            let directory = tempfile::tempdir().expect("temporary store directory");
            let config = store_config(directory.path().join("hiqlite"));
            let store = match Store::open(&config).await {
                Ok(store) => store,
                Err(error) if error.message().contains("Address already in use") => {
                    last = Some(error);
                    continue;
                }
                Err(error) => panic!("single-voter store: {error}"),
            };
            let migrated = async {
                store.handle().migrate(Aicortex::migrations()).await?;
                store
                    .handle()
                    .migrate_sets(&[], &Aicortex::migration_sets())
                    .await
            }
            .await;
            match migrated {
                Ok(_) => {
                    return Self {
                        store,
                        _directory: directory,
                    };
                }
                Err(error) if error.message().contains("request timed out") => {
                    let _ = store.shutdown().await;
                    last = Some(error);
                }
                Err(error) => panic!("the cell's migrations apply: {error}"),
            }
        }
        panic!("the fixture store is not ready: {last:?}")
    }

    fn handle(&self) -> StoreHandle {
        self.store.handle()
    }

    async fn shutdown(self) {
        self.store.shutdown().await.expect("the node stops");
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

async fn insert_memory(store: &StoreHandle, memory: &Memory) {
    store
        .execute(
            "INSERT INTO scope (scope_id, owner, kind, key, created)
             VALUES (?1, 'alice', 'personal', '', 1) ON CONFLICT DO NOTHING",
            vec![Value::from(SCOPE)],
        )
        .await
        .expect("scope inserts");
    store
        .execute(
            "INSERT INTO memory (id, scope_id, status, kind, trust, fingerprint, schema_version,
                                 body_bytes, created, updated, record)
             VALUES (?1, ?2, 'active', 'observation', 'assertion', ?1, 1, 10, 2, 2, ?3)",
            vec![
                Value::from(memory.id.to_string()),
                Value::from(SCOPE),
                Value::from(serde_json::to_string(memory).expect("memory serializes")),
            ],
        )
        .await
        .expect("memory inserts");
}

async fn verdict(store: &StoreHandle) -> rahi_cli::AppVerdict {
    graph_preflight::verdict(store, Duration::from_secs(5)).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ac2_a_fresh_deployment_reports_an_empty_graph() {
    let fixture = Fixture::migrated().await;
    let report = verdict(&fixture.handle()).await;
    assert_eq!(report.outcome, AppOutcome::Pass, "{report:?}");
    assert!(
        report
            .detail
            .contains("0 entities, 0 edges, 0 extraction dead letters")
    );
    fixture.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ac2_counts_per_scope_and_a_dead_letter_warns() {
    let fixture = Fixture::migrated().await;
    let store = fixture.handle();
    let stored = memory("Dana Whitfield works on Apollo.");
    insert_memory(&store, &stored).await;

    // The service stages the job and writes the graph rows; stopping it is
    // part of the contract, so the same run proves it returns Ok.
    let worker = ExtractionWorker::new(
        PatternExtractor,
        ExtractionConfig::standard(),
        "app-test-worker",
    )
    .expect("a worker");
    let (stop, stopped) = watch::channel(false);
    let service = tokio::spawn(graph_service::work(worker, store.clone(), move || {
        let mut stopped = stopped.clone();
        async move {
            let _ = stopped.wait_for(|stop| *stop).await;
        }
    }));
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let graph = read_scope(&store, SCOPE).await.expect("scope reads");
        if graph.entities == 2 && graph.edges == 1 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the graph never appeared: {graph:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    stop.send(true).expect("the service is listening");
    service
        .await
        .expect("the service did not panic")
        .expect("the service returns Ok on shutdown");

    let report = verdict(&store).await;
    assert_eq!(report.outcome, AppOutcome::Pass, "{report:?}");
    assert!(
        report
            .detail
            .contains("2 entities, 1 edges, 0 extraction dead letters")
    );
    assert!(
        report
            .report
            .iter()
            .any(|line| line.contains(SCOPE) && line.contains("entities=2 edges=1")),
        "{report:?}"
    );

    // A job that exhausts its attempts is a dead letter, and a warning.
    let second = memory("Erin Park works on Hermes.");
    insert_memory(&store, &second).await;
    let mut txn = TxnBuilder::new();
    aicortex_store::stage_extraction(
        &mut txn,
        SCOPE,
        second.id,
        PatternExtractor::NAME,
        PatternExtractor::VERSION,
        UnixSeconds::new(10),
    )
    .expect("the work stages");
    store.txn(txn.into_statements()).await.expect("commits");
    let claim = Work::next(
        &store,
        aicortex_store::EMBEDDING_NAMESPACE,
        &aicortex_store::extraction_processor(PatternExtractor::VERSION),
        "app-test",
        Duration::from_secs(60),
        UnixSeconds::new(11),
        1,
    )
    .await
    .expect("claims")
    .pop()
    .expect("one claim");
    let mut txn = TxnBuilder::new();
    Work::fail(
        &mut txn,
        &claim,
        &FailureDetail {
            class: "test".to_owned(),
            detail: None,
        },
        &RetryPolicy {
            max_attempts: 1,
            base: Duration::from_secs(1),
            cap: Duration::from_secs(1),
        },
        UnixSeconds::new(12),
    )
    .expect("fails");
    store.txn(txn.into_statements()).await.expect("commits");
    let warned = verdict(&store).await;
    assert_eq!(warned.outcome, AppOutcome::Warn, "{warned:?}");
    assert!(
        warned.detail.contains("1 extraction dead letters"),
        "{warned:?}"
    );
    fixture.shutdown().await;
}
