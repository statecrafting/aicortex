//! The fixture the ingest tests share (spec 030).
//!
//! A real single-voter rahi node in a throwaway directory, migrated with the
//! application schema, the chassis's real decision chain on top of it, the
//! real write gate, and a stub source over a fixture of items. Every
//! assertion reads the store back; nothing inspects what the code intended to
//! write.

#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::net::{SocketAddr, TcpListener};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use aicortex_api::Events;
use aicortex_gate::Gate;
use aicortex_ingest::{
    AdapterContext, AdapterError, AdapterResult, BoxFuture, Candidate, Cursor, IngestPolicy, Page,
    Probe, RawItem, Registry, RunLimits, RunRequest, Runner, Runtime, SourceAdapter, SourceId,
    SourceKind,
};
use aicortex_types::{Memory, MemoryKind, Scope};
use rahi_kernel::{Manifest, ServiceName};
use rahi_ledger::{Hash, Ledger, LedgerSigner};
use rahi_store::{EncKey, EncKeys, Store, StoreConfig, StoreHandle, StoreSecrets, Value};
use rahi_types::{Sub, UnixSeconds};
use serde::Deserialize;
use tokio::sync::Notify;

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

    /// A chain whose node has been stopped, so every append to it fails.
    pub async fn broken_ledger(&self) -> Ledger {
        let other = node().await;
        let ledger = other.ledger.clone();
        other.shutdown().await;
        ledger
    }

    pub fn runtime(&self) -> Runtime {
        self.runtime_with(self.ledger.clone())
    }

    pub fn runtime_with(&self, ledger: Ledger) -> Runtime {
        Runtime {
            store: self.handle(),
            ledger,
            gate: Arc::new(Gate::standard()),
            events: Events::new(),
        }
    }

    /// One integer out of a `SELECT COUNT(*) AS count` statement.
    pub async fn count(&self, sql: &'static str, params: Vec<Value>) -> u64 {
        #[derive(Deserialize)]
        struct Count {
            count: i64,
        }
        let rows: Vec<Count> = self.handle().query_consistent(sql, params).await.unwrap();
        u64::try_from(rows[0].count).unwrap()
    }

    /// Run one statement outside the code under test.
    pub async fn exec(&self, sql: &'static str, params: Vec<Value>) {
        self.handle()
            .txn(vec![rahi_store::Statement::with_params(sql, params)])
            .await
            .expect("the statement runs");
    }

    /// Every stored memory of a scope, in no particular order.
    pub async fn memories(&self, scope: &Scope) -> Vec<Memory> {
        #[derive(Deserialize)]
        struct Row {
            record: String,
        }
        let rows: Vec<Row> = self
            .handle()
            .query_consistent(
                "SELECT record FROM memory WHERE scope_id = $1",
                vec![Value::from(&aicortex_store::ScopeId::of(scope))],
            )
            .await
            .unwrap();
        rows.into_iter()
            .map(|row| serde_json::from_str(&row.record).unwrap())
            .collect()
    }

    /// The bodies of a scope's memories that are in `status`, sorted.
    pub async fn bodies(&self, scope: &Scope, status: &str) -> Vec<String> {
        let mut bodies: Vec<String> = self
            .memories(scope)
            .await
            .into_iter()
            .filter(|memory| memory.status.label() == status)
            .map(|memory| memory.body.text)
            .collect();
        bodies.sort();
        bodies
    }

    pub async fn memory_rows(&self, scope: &Scope) -> u64 {
        self.count(
            "SELECT COUNT(*) AS count FROM memory WHERE scope_id = $1",
            vec![Value::from(&aicortex_store::ScopeId::of(scope))],
        )
        .await
    }

    pub async fn item_rows(&self, scope: &Scope) -> u64 {
        self.count(
            "SELECT COUNT(*) AS count FROM ingest_item WHERE scope_id = $1",
            vec![Value::from(&aicortex_store::ScopeId::of(scope))],
        )
        .await
    }

    /// The saved cursor of a source, if any.
    pub async fn cursor(&self, scope: &Scope, source: &str) -> Option<String> {
        #[derive(Deserialize)]
        struct Row {
            cursor: String,
        }
        let rows: Vec<Row> = self
            .handle()
            .query_consistent(
                "SELECT cursor FROM ingest_cursor WHERE scope_id = $1 AND source_id = $2",
                vec![
                    Value::from(&aicortex_store::ScopeId::of(scope)),
                    Value::from(source),
                ],
            )
            .await
            .unwrap();
        rows.into_iter().next().map(|row| row.cursor)
    }
}

/// A migrated node with a chain at genesis.
pub async fn node() -> Node {
    node_over(&Hash::parse(format!("sha256:{}", "cd".repeat(32))).expect("a manifest hash")).await
}

/// A migrated node whose chain commits to `manifest`, so a kernel can boot
/// on it.
pub async fn node_for(manifest: &Manifest) -> Node {
    node_over(&manifest.hash().expect("the manifest hashes")).await
}

async fn node_over(genesis: &Hash) -> Node {
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
                    genesis.clone(),
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
            secret_raft: "raft-secret-for-ingest-tests".to_owned(),
            secret_api: "api-secret-for-ingest-tests".to_owned(),
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

/// A manifest that grants the `ingest` service egress to `hosts`.
pub fn manifest(hosts: &[&str]) -> Manifest {
    let declared = hosts
        .iter()
        .map(|host| format!("\"{host}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let grants = hosts
        .iter()
        .enumerate()
        .map(|(index, host)| {
            format!(
                "\n[[capabilities]]\nid = \"ingest-egress-{index}\"\nkind = \"http.egress\"\n\
                 resource = \"{host}\"\n"
            )
        })
        .collect::<String>();
    let ids = (0..hosts.len())
        .map(|index| format!("\"ingest-egress-{index}\""))
        .collect::<Vec<_>>()
        .join(", ");
    Manifest::parse(&format!(
        r#"
schema_version = "1.0.0"

[app]
name = "aicortex"
org = "statecrafting"

[resources]
egress = [{declared}]
{grants}
[services.ingest]
capabilities = [{ids}]

[ledger]
schema_version = "1.0.0"
max_record_bytes = 65536

[observability]
metrics_path = "/metrics"
otel = false

[auth]
operator_role = "aicortex_operator"

[contract]
version = "0.1.0"
"#
    ))
    .expect("the manifest parses")
}

pub fn ingest_service() -> ServiceName {
    ServiceName::parse(aicortex_ingest::INGEST_SERVICE).expect("the service name")
}

/// What the fixture says about item `index`.
pub fn text(index: usize) -> String {
    format!("fixture note {index:04}: the meeting about subject {index} moved to room {index}")
}

/// A fixture of `count` distinct items.
pub fn fixture(count: usize) -> Vec<RawItem> {
    (0..count)
        .map(|index| {
            RawItem::new(
                format!("ext-{index:04}"),
                UnixSeconds::new(1_700_000_000 + index as u64),
                serde_json::json!({ "text": text(index) }),
            )
            .with_locator(format!("fixture://notes/{index}"))
        })
        .collect()
}

/// What a stub does on a page, besides serving it.
#[derive(Clone, Default)]
pub struct Faults {
    /// The page index that fails transiently, every time it is asked for.
    pub transient_at: Option<usize>,
    /// The page index that fails for good.
    pub fatal_at: Option<usize>,
    /// The page index at which the stub announces itself and waits to be let
    /// go: how a test stands between two pages.
    pub park_at: Option<usize>,
    /// The page index that returns more items than were asked for.
    pub oversized_at: Option<usize>,
    /// The page index that returns a cursor equal to the one it was given.
    pub stuck_at: Option<usize>,
    /// Park forever instead of waiting to be let go.
    pub park_forever: bool,
    /// From this page index on, return no items and a new cursor, forever.
    pub empty_from: Option<usize>,
}

/// A source over a fixture: pages of the asked-for length, cursors that are
/// the next index.
pub struct Stub {
    pub id: SourceId,
    pub kind: SourceKind,
    pub items: Vec<RawItem>,
    pub hosts: Vec<String>,
    pub faults: Mutex<Faults>,
    /// The page limits the runner asked for, in order.
    pub limits: Mutex<Vec<usize>>,
    pub served: AtomicUsize,
    /// Signalled when the stub reaches its park page.
    pub reached: Arc<Notify>,
    /// Waited on at the park page.
    pub release: Arc<Notify>,
}

impl Stub {
    pub fn new(id: &str, kind: SourceKind, items: Vec<RawItem>) -> Arc<Self> {
        Arc::new(Self {
            id: SourceId::new(id).expect("a source id"),
            kind,
            items,
            hosts: Vec::new(),
            faults: Mutex::new(Faults::default()),
            limits: Mutex::new(Vec::new()),
            served: AtomicUsize::new(0),
            reached: Arc::new(Notify::new()),
            release: Arc::new(Notify::new()),
        })
    }

    pub fn with_hosts(id: &str, kind: SourceKind, hosts: &[&str]) -> Arc<Self> {
        Arc::new(Self {
            id: SourceId::new(id).expect("a source id"),
            kind,
            items: Vec::new(),
            hosts: hosts.iter().map(|host| (*host).to_owned()).collect(),
            faults: Mutex::new(Faults::default()),
            limits: Mutex::new(Vec::new()),
            served: AtomicUsize::new(0),
            reached: Arc::new(Notify::new()),
            release: Arc::new(Notify::new()),
        })
    }

    pub fn set_faults(&self, faults: Faults) {
        *self.faults.lock().unwrap() = faults;
    }

    pub fn limits(&self) -> Vec<usize> {
        self.limits.lock().unwrap().clone()
    }
}

impl SourceAdapter for Stub {
    fn id(&self) -> SourceId {
        self.id.clone()
    }

    fn kind(&self) -> SourceKind {
        self.kind
    }

    fn hosts(&self) -> Vec<String> {
        self.hosts.clone()
    }

    fn probe<'a>(&'a self, _ctx: &'a AdapterContext) -> BoxFuture<'a, AdapterResult<Probe>> {
        Box::pin(async move {
            Ok(Probe {
                label: "fixture".to_owned(),
                estimated_items: Some(self.items.len() as u64),
            })
        })
    }

    fn page<'a>(
        &'a self,
        ctx: &'a AdapterContext,
        cursor: Option<&'a Cursor>,
    ) -> BoxFuture<'a, AdapterResult<Page>> {
        Box::pin(async move {
            let faults = self.faults.lock().unwrap().clone();
            let start = cursor.map_or(0, |cursor| cursor.as_str().parse::<usize>().unwrap());
            let limit = ctx.page_limit();
            self.limits.lock().unwrap().push(limit);
            let index = self.served.fetch_add(1, Ordering::SeqCst);
            let page_index = start / 100;
            if faults.park_at == Some(page_index) {
                self.reached.notify_one();
                if faults.park_forever {
                    std::future::pending::<()>().await;
                }
                self.release.notified().await;
            }
            if faults.transient_at == Some(page_index) {
                return Err(AdapterError::Transient(format!("page {index} timed out")));
            }
            if faults.fatal_at == Some(page_index) {
                return Err(AdapterError::Fatal("the credential was refused".to_owned()));
            }
            if faults.empty_from.is_some_and(|from| page_index >= from) {
                return Ok(Page {
                    items: Vec::new(),
                    next: Some(Cursor::new(format!("{}", start + 1)).unwrap()),
                });
            }
            let take = if faults.oversized_at == Some(page_index) {
                limit + 1
            } else {
                limit
            };
            let end = (start + take).min(self.items.len());
            let items = self.items[start..end].to_vec();
            let next = if end >= self.items.len() {
                None
            } else if faults.stuck_at == Some(page_index) {
                Some(Cursor::new(start.to_string()).unwrap())
            } else {
                Some(Cursor::new(end.to_string()).unwrap())
            };
            Ok(Page { items, next })
        })
    }

    fn map(&self, item: &RawItem) -> AdapterResult<Candidate> {
        let text = item
            .payload
            .get("text")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| AdapterError::Fatal("the item has no text".to_owned()))?;
        let candidate = Candidate::new(MemoryKind::Observation, text);
        Ok(
            match item
                .payload
                .get("title")
                .and_then(serde_json::Value::as_str)
            {
                Some(title) => candidate.with_title(title),
                None => candidate,
            },
        )
    }
}

/// A runner over `stubs`, registered under a manifest that grants no egress.
pub fn runner(stubs: &[Arc<Stub>], policy: IngestPolicy) -> Runner {
    let mut registry = Registry::new();
    let manifest = manifest(&[]);
    for stub in stubs {
        registry
            .register_under(stub.clone(), &manifest, &ingest_service())
            .expect("the stub registers");
    }
    Runner::new(Arc::new(registry), None, policy)
}

/// Limits that do not slow a test down.
pub fn quick() -> RunLimits {
    RunLimits {
        max_items: 100_000,
        rate_per_second: 100_000,
        retries: 2,
        backoff: std::time::Duration::ZERO,
    }
}

pub fn request(scope: &Scope, source: &str) -> RunRequest {
    RunRequest {
        scope: scope.clone(),
        source: SourceId::new(source).expect("a source id"),
        principal: Sub::new("alice"),
        options: serde_json::Value::Null,
        limits: quick(),
        dry_run: false,
    }
}
