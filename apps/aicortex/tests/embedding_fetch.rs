//! Spec 015 B-5 and D-32: the one-time model fetch at `serve` boot.
//!
//! A real kernel admits the host and a real TLS mirror serves the artifacts,
//! so these tests exercise the path a deployment takes: the permit, the
//! HTTPS client of `embedding_fetch.rs`, the pin, and the worker that binds
//! only after both artifacts are installed.

#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used
)]

use std::collections::BTreeMap;
use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use aicortex::Aicortex;
use aicortex::embedding_fetch::{HttpsFetcher, WeightFetch, needs_provisioning};
use aicortex::embedding_service;
use aicortex_embed::static_model::EMBEDDINGS_TENSOR;
use aicortex_embed::{EmbeddingConfig, NoTransport, WeightFetcher, operator};
use axum::handler::HandlerWithoutStateExt;
use axum::http::{StatusCode, Uri, header};
use axum::response::IntoResponse;
use rahi_cli::Cell as _;
use rahi_kernel::{CapabilityKind, Egress, Governed, Kernel, Manifest};
use rahi_ledger::{Ledger, LedgerSigner};
use rahi_store::{EncKey, EncKeys, Store, StoreConfig, StoreHandle, StoreSecrets};
use rahi_types::{Error, Sub, UnixSeconds};
use rcgen::{BasicConstraints, CertificateParams, CertifiedIssuer, IsCa, KeyPair};
use ring::digest::{SHA256, digest};
use safetensors::Dtype;
use safetensors::tensor::TensorView;
use tokio::sync::watch;

/// The mirror's host. It never resolves through DNS: the fetcher pins it to
/// the loopback mirror.
const HOST: &str = "models.example";

const TOKENIZER: &str = r#"{
  "normalizer": {"type": "BertNormalizer", "lowercase": true},
  "pre_tokenizer": {"type": "BertPreTokenizer"},
  "model": {"type": "WordPiece", "unk_token": "[UNK]",
            "vocab": {"[UNK]": 0, "red": 1, "green": 2}}
}"#;

fn free_addr() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("free loopback port");
    listener.local_addr().expect("bound address")
}

fn sha(bytes: &[u8]) -> String {
    digest(&SHA256, bytes)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn weights() -> Vec<u8> {
    let data: Vec<u8> = [9.0_f32, 9.0, 1.0, 0.0, 0.0, 1.0]
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect();
    let view = TensorView::new(Dtype::F32, vec![3, 2], &data).expect("view");
    safetensors::serialize([(EMBEDDINGS_TENSOR, view)], None).expect("serialize")
}

/// A loopback HTTPS mirror for [`HOST`], counting the requests it answers.
struct Mirror {
    address: SocketAddr,
    root_pem: String,
    hits: Arc<AtomicUsize>,
    handle: axum_server::Handle<SocketAddr>,
}

impl Mirror {
    async fn start() -> Self {
        // Both of rustls's providers are in the tree, so a server picks none
        // on its own; a second install in the same binary is a no-op.
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let mut ca_params = CertificateParams::new(Vec::<String>::new()).expect("ca params");
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        let ca = CertifiedIssuer::self_signed(ca_params, KeyPair::generate().expect("ca key"))
            .expect("ca certificate");
        let leaf_key = KeyPair::generate().expect("leaf key");
        let leaf = CertificateParams::new(vec![HOST.to_owned()])
            .expect("leaf params")
            .signed_by(&leaf_key, &ca)
            .expect("leaf certificate");
        let tls = axum_server::tls_rustls::RustlsConfig::from_pem(
            leaf.pem().into_bytes(),
            leaf_key.serialize_pem().into_bytes(),
        )
        .await
        .expect("tls configuration");

        let hits = Arc::new(AtomicUsize::new(0));
        // One handler answers every path, so the fixture builds no router
        // (spec 010 B-8 keeps routers to the cell and the surface crates).
        let counter = Arc::clone(&hits);
        let app = move |uri: Uri| {
            let hits = Arc::clone(&counter);
            async move {
                let body = match uri.path() {
                    "/m.safetensors" => weights(),
                    "/tokenizer.json" => TOKENIZER.as_bytes().to_vec(),
                    "/tampered.json" => b"{}".to_vec(),
                    "/large" => vec![0_u8; 4096],
                    "/moved" => {
                        return (
                            StatusCode::FOUND,
                            [(header::LOCATION, "https://elsewhere.example/m.safetensors")],
                        )
                            .into_response();
                    }
                    _ => return StatusCode::NOT_FOUND.into_response(),
                };
                hits.fetch_add(1, Ordering::SeqCst);
                body.into_response()
            }
        };
        let listener = TcpListener::bind("127.0.0.1:0").expect("mirror port");
        listener.set_nonblocking(true).expect("non-blocking");
        let address = listener.local_addr().expect("mirror address");
        let handle = axum_server::Handle::new();
        let server = axum_server::from_tcp_rustls(listener, tls)
            .expect("mirror server")
            .handle(handle.clone());
        tokio::spawn(async move {
            let _ = server.serve(app.into_make_service()).await;
        });
        Self {
            address,
            root_pem: ca.pem(),
            hits,
            handle,
        }
    }

    fn fetcher(&self) -> HttpsFetcher {
        HttpsFetcher::for_mirror(self.root_pem.as_bytes(), HOST, self.address)
            .expect("mirror fetcher")
    }

    fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }
}

impl Drop for Mirror {
    fn drop(&mut self) {
        self.handle.shutdown();
    }
}

fn manifest(egress: &[&str]) -> Manifest {
    let hosts = egress
        .iter()
        .map(|host| format!("\"{host}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let grant = egress.first().map_or_else(String::new, |host| {
        format!(
            r#"
[[capabilities]]
id = "embedding-egress"
kind = "http.egress"
resource = "{host}"
"#
        )
    });
    let services = if egress.is_empty() {
        "[services.embedding]\ncapabilities = []\n"
    } else {
        "[services.embedding]\ncapabilities = [\"embedding-egress\"]\n"
    };
    Manifest::parse(&format!(
        r#"
schema_version = "1.0.0"

[app]
name = "aicortex"
org = "statecrafting"

[resources]
egress = [{hosts}]
{grant}
{services}
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
    .expect("manifest parses")
}

/// A migrated single-voter node and a kernel booted against `manifest`.
struct Booted {
    kernel: Kernel,
    store: Store,
    directory: tempfile::TempDir,
}

impl Booted {
    async fn boot(manifest: Manifest) -> Self {
        let directory = tempfile::tempdir().expect("temporary store directory");
        let config = StoreConfig {
            node_id: 1,
            nodes: Vec::new(),
            data_dir: directory.path().join("hiqlite"),
            raft_addr: free_addr(),
            api_addr: free_addr(),
            secrets: StoreSecrets {
                secret_raft: "raft-secret-for-fetch-tests".to_owned(),
                secret_api: "api-secret-for-fetch-tests".to_owned(),
                enc_keys: EncKeys {
                    active: "test".to_owned(),
                    keys: vec![EncKey {
                        id: "test".to_owned(),
                        key: vec![29_u8; 32],
                    }],
                },
            },
            backup_keep_days: 1,
            s3: None,
        };
        let store = Store::open(&config).await.expect("single-voter store");
        let handle = store.handle();
        handle
            .migrate(Aicortex::migrations())
            .await
            .expect("the cell's migrations apply");
        handle
            .migrate_sets(&[], &Aicortex::migration_sets())
            .await
            .expect("the chassis sets apply");
        let ledger = Ledger::open(
            handle.clone(),
            LedgerSigner::from_seed([5_u8; 32]),
            manifest.hash().expect("manifest hashes"),
        )
        .await
        .expect("the chain opens");
        let kernel = Kernel::boot(manifest, handle, ledger)
            .await
            .expect("the kernel boots");
        Self {
            kernel,
            store,
            directory,
        }
    }

    fn handle(&self) -> StoreHandle {
        self.store.handle()
    }

    fn models_dir(&self) -> PathBuf {
        self.directory.path().join("models")
    }

    fn fetch(&self, mirror: &Mirror) -> WeightFetch {
        let egress = Governed::new(
            &self.kernel,
            aicortex_embed::EMBEDDING_SERVICE,
            CapabilityKind::HttpEgress,
            "*",
            Egress,
        )
        .expect("the manifest declares the embedding service");
        WeightFetch::new(egress, mirror.fetcher())
    }

    async fn shutdown(self) {
        drop(self.kernel);
        self.store.shutdown().await.expect("the node stops");
    }
}

/// A local configuration whose artifacts are absent and pinned to the
/// mirror's bytes, with the tokenizer served from `tokenizer_path`.
fn absent_env(models: &Path, tokenizer_path: &str) -> BTreeMap<String, String> {
    [
        ("AICORTEX_EMBED_MODEL", "static-fixture"),
        ("AICORTEX_EMBED_DIMS", "2"),
        ("AICORTEX_EMBED_MODELS_DIR", models.to_str().expect("utf-8")),
        ("AICORTEX_EMBED_WEIGHTS_FILE", "model.safetensors"),
        (
            "AICORTEX_EMBED_WEIGHTS_URL",
            &format!("https://{HOST}/m.safetensors"),
        ),
        ("AICORTEX_EMBED_WEIGHTS_SHA256", &sha(&weights())),
        ("AICORTEX_EMBED_TOKENIZER_FILE", "tokenizer.json"),
        (
            "AICORTEX_EMBED_TOKENIZER_URL",
            &format!("https://{HOST}{tokenizer_path}"),
        ),
        (
            "AICORTEX_EMBED_TOKENIZER_SHA256",
            &sha(TOKENIZER.as_bytes()),
        ),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_owned(), value.to_owned()))
    .collect()
}

fn stop_signal() -> (
    watch::Sender<bool>,
    impl Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
    + Clone
    + Send
    + Sync
    + 'static,
) {
    let (sender, receiver) = watch::channel(false);
    let stop = move || {
        let mut receiver = receiver.clone();
        Box::pin(async move {
            let _ = receiver.wait_for(|stopped| *stopped).await;
        }) as std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
    };
    (sender, stop)
}

async fn eventually<F, Fut>(what: &str, mut condition: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = Instant::now() + Duration::from_secs(40);
    while !condition().await {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// B-5: absent artifacts are fetched through the admitted host, verified
/// against their pins, and installed; a second pass fetches nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b5_absent_artifacts_are_fetched_once_and_verified() {
    let mirror = Mirror::start().await;
    let cell = Booted::boot(manifest(&[HOST])).await;
    let config = EmbeddingConfig::from_env(&absent_env(&cell.models_dir(), "/tokenizer.json"))
        .expect("configuration parses");
    assert!(needs_provisioning(&config));

    let fetch = cell.fetch(&mirror);
    assert_eq!(
        fetch.provision(&config).await.expect("the fetch succeeds"),
        2
    );
    assert_eq!(mirror.hits(), 2);
    assert!(!needs_provisioning(&config));
    assert_eq!(
        std::fs::read(cell.models_dir().join("model.safetensors")).expect("installed"),
        weights()
    );

    assert_eq!(fetch.provision(&config).await.expect("nothing to do"), 0);
    assert_eq!(mirror.hits(), 2, "a present, valid artifact is not fetched");
    cell.shutdown().await;
}

/// B-5: bytes that do not match the pin are refused and never installed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b5_bytes_that_miss_the_pin_are_not_installed() {
    let mirror = Mirror::start().await;
    let cell = Booted::boot(manifest(&[HOST])).await;
    let config = EmbeddingConfig::from_env(&absent_env(&cell.models_dir(), "/tampered.json"))
        .expect("configuration parses");

    let error = cell
        .fetch(&mirror)
        .provision(&config)
        .await
        .expect_err("a tampered artifact is refused");
    assert!(matches!(error, Error::Integrity(_)), "{error:?}");
    assert!(!cell.models_dir().join("tokenizer.json").exists());
    cell.shutdown().await;
}

/// B-6 for the fetch: a host the ceiling does not grant is refused by the
/// kernel before any request leaves the process.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b5_an_ungranted_host_is_denied_before_any_request() {
    let mirror = Mirror::start().await;
    let cell = Booted::boot(manifest(&[])).await;
    let config = EmbeddingConfig::from_env(&absent_env(&cell.models_dir(), "/tokenizer.json"))
        .expect("configuration parses");

    let error = cell
        .fetch(&mirror)
        .provision(&config)
        .await
        .expect_err("the kernel refuses the host");
    assert!(matches!(error, Error::Denied { .. }), "{error:?}");
    assert_eq!(mirror.hits(), 0, "no request reached the mirror");
    cell.shutdown().await;
}

/// The transport refuses a redirect and stops reading past the bound.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_transport_refuses_redirects_and_oversized_bodies() {
    let mirror = Mirror::start().await;
    let cell = Booted::boot(manifest(&[HOST])).await;
    let egress = Governed::new(
        &cell.kernel,
        aicortex_embed::EMBEDDING_SERVICE,
        CapabilityKind::HttpEgress,
        "*",
        Egress,
    )
    .expect("facade");
    let permit = egress
        .permit(&Sub::new("test"), HOST)
        .await
        .expect("the host is granted");
    let fetcher = mirror.fetcher();

    let moved = fetcher
        .fetch(&permit, "/moved", 1 << 20)
        .await
        .expect_err("a redirect is not followed");
    assert!(matches!(moved, Error::Io(_)), "{moved:?}");

    let large = fetcher
        .fetch(&permit, "/large", 1024)
        .await
        .expect_err("a body over the bound is refused");
    assert!(matches!(large, Error::Integrity(_)), "{large:?}");

    let small = fetcher
        .fetch(&permit, "/tokenizer.json", 1 << 20)
        .await
        .expect("a body under the bound is returned");
    assert_eq!(small, TOKENIZER.as_bytes());
    cell.shutdown().await;
}

/// B-2 with B-5: a worker configured with absent artifacts fetches them
/// before anything binds, the fetched model activates, and the worker still
/// stops only on shutdown.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn b2_b5_the_worker_fetches_before_it_binds() {
    let mirror = Mirror::start().await;
    let manifest = manifest(&[HOST]);
    let cell = Booted::boot(manifest.clone()).await;
    let store = cell.handle();
    let config = EmbeddingConfig::from_env(&absent_env(&cell.models_dir(), "/tokenizer.json"))
        .expect("configuration parses");
    config
        .check(&manifest)
        .expect("an absent artifact passes when its host is granted");
    let (sender, stop) = stop_signal();
    let running = tokio::spawn(embedding_service::work(
        config.clone(),
        store.clone(),
        Some(cell.fetch(&mirror)),
        stop,
    ));

    eventually("both artifacts to be installed", || {
        let config = config.clone();
        async move { !needs_provisioning(&config) }
    })
    .await;
    let activation = operator::activate_configured(
        &config,
        &manifest,
        None,
        &Sub::new("test-operator"),
        NoTransport,
        &store,
        UnixSeconds::new(5),
    )
    .await
    .expect("the configured model activates");
    assert_eq!(activation.model.revision, 1);
    assert!(
        !running.is_finished(),
        "the worker returned before shutdown"
    );

    sender.send_replace(true);
    let outcome = tokio::time::timeout(Duration::from_secs(20), running)
        .await
        .expect("the worker stops on shutdown")
        .expect("the worker does not panic");
    assert!(outcome.is_ok(), "{outcome:?}");
    assert_eq!(mirror.hits(), 2);
    cell.shutdown().await;
}

/// D-32: a fetch that cannot succeed (bytes that miss the pin) is retried,
/// never installs anything, and does not keep the worker from stopping.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn d32_a_failing_fetch_is_retried_and_the_worker_still_stops() {
    let mirror = Mirror::start().await;
    let cell = Booted::boot(manifest(&[HOST])).await;
    let config = EmbeddingConfig::from_env(&absent_env(&cell.models_dir(), "/tampered.json"))
        .expect("configuration parses");
    let (sender, stop) = stop_signal();
    let running = tokio::spawn(embedding_service::work(
        config.clone(),
        cell.handle(),
        Some(cell.fetch(&mirror)),
        stop,
    ));

    // The weights install; the tampered tokenizer is fetched and refused.
    eventually("the tokenizer fetch to be attempted", || {
        let hits = mirror.hits();
        async move { hits >= 2 }
    })
    .await;
    assert!(
        !running.is_finished(),
        "the worker returned before shutdown"
    );
    assert!(needs_provisioning(&config));
    assert!(!cell.models_dir().join("tokenizer.json").exists());

    sender.send_replace(true);
    let outcome = tokio::time::timeout(Duration::from_secs(20), running)
        .await
        .expect("the worker stops during the backoff")
        .expect("the worker does not panic");
    assert!(outcome.is_ok(), "{outcome:?}");
    cell.shutdown().await;
}
