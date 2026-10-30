//! The cell the API tests run against: the real chassis on a real socket.
//!
//! A single-voter store, the decision chain, and the kernel, as `serve`
//! builds them; the API router mounted into rahi's edge with the stream hub;
//! and the chassis's own bearer layer over a resource server whose key set is
//! fetched from a loopback stub. Every token is a real RS256 JWT, signed here
//! with a key generated for the run and verified by the chassis against the
//! key set it fetched, so the authorization under test is the chassis's and
//! not a stand-in. Only rauthy itself is absent; AC-2 is the run against it.

#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::collections::BTreeMap;
use std::net::{SocketAddr, TcpListener as StdListener};
use std::path::Path;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use aicortex_embed::{ModelId, ModelRegistry, ModelRevision};
use axum::Router;
use axum::http::HeaderMap;
use axum::routing::get;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rahi_edge::{AppState, Edge, StreamHub, StreamOptions};
use rahi_idp::{Discovery, IdpConfig, Jwks, RequireBearer, Resource, ResourceServer, with_bearer};
use rahi_kernel::{Kernel, Manifest};
use rahi_ledger::{Ledger, LedgerSigner, SignedRecord};
use rahi_store::{EncKey, EncKeys, Store, StoreConfig, StoreSecrets, TxnBuilder};
use rahi_types::{Config, UnixSeconds};
use ring::rand::SystemRandom;
use ring::rsa::PublicKeyComponents;
use ring::signature::{RSA_PKCS1_SHA256, RsaKeyPair};
use serde_json::{Value, json};

/// The client id every person's token names unless a test says otherwise.
pub const CLIENT: &str = "test-client";
/// The key id the stub publishes.
pub const KID: &str = "aicortex-api-test";
/// The keep-alive interval the test hub uses, so FR-007 waits milliseconds.
pub const KEEP_ALIVE: Duration = Duration::from_millis(100);

/// The smallest manifest the kernel boots.
const MANIFEST: &str = r#"
schema_version = "1.0.0"
capabilities = []

[app]
name = "aicortex"
org = "statecrafting"

[resources]
tables = []
kv = []
counters = []
secrets = []
egress = []

[services]

[gate]
checks = []

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
"#;

/// The run's signing key: generated once, because RSA generation is slow.
fn signing_key() -> &'static (RsaKeyPair, Value) {
    static KEY: OnceLock<(RsaKeyPair, Value)> = OnceLock::new();
    KEY.get_or_init(|| {
        let generated =
            rcgen::KeyPair::generate_for(&rcgen::PKCS_RSA_SHA256).expect("an RSA key generates");
        let pair =
            RsaKeyPair::from_pkcs8(&generated.serialize_der()).expect("the key reads as PKCS#8");
        let public = PublicKeyComponents::<Vec<u8>>::from(pair.public());
        let jwk = json!({
            "kty": "RSA",
            "alg": "RS256",
            "use": "sig",
            "kid": KID,
            "n": URL_SAFE_NO_PAD.encode(&public.n),
            "e": URL_SAFE_NO_PAD.encode(&public.e),
        });
        (pair, jwk)
    })
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn free_addr() -> SocketAddr {
    let listener = StdListener::bind("127.0.0.1:0").expect("a free port");
    listener.local_addr().expect("the port it took")
}

fn store_config(data_dir: &Path) -> StoreConfig {
    StoreConfig {
        node_id: 1,
        nodes: Vec::new(),
        data_dir: data_dir.to_path_buf(),
        raft_addr: free_addr(),
        api_addr: free_addr(),
        secrets: StoreSecrets {
            secret_raft: "raft-secret-for-aicortex-api-tests".to_owned(),
            secret_api: "api-secret-for-aicortex-api-tests".to_owned(),
            enc_keys: EncKeys {
                active: "test".to_owned(),
                keys: vec![EncKey {
                    id: "test".to_owned(),
                    key: vec![11_u8; 32],
                }],
            },
        },
        backup_keep_days: 1,
        s3: None,
    }
}

/// Open a single-voter store on free ports, retrying a port collision in a
/// fresh directory.
async fn open_store() -> (Store, tempfile::TempDir) {
    let mut last = None;
    for _ in 0..5 {
        let dir = tempfile::tempdir().expect("a temporary directory");
        match Store::open(&store_config(&dir.path().join("hiqlite"))).await {
            Ok(store) => return (store, dir),
            Err(error) if error.message().contains("Address already in use") => {
                last = Some(error);
            }
            Err(error) => panic!("a single-voter node opens: {error}"),
        }
    }
    panic!("no free ports after bounded retries: {last:?}")
}

/// A booted cell serving the API.
pub struct Cell {
    pub store: Store,
    pub ledger: Ledger,
    pub kernel: Kernel,
    pub hub: StreamHub,
    pub origin: String,
    pub http: reqwest::Client,
    issuer: String,
    dir: tempfile::TempDir,
}

/// Boot the cell with the default limits.
pub async fn boot() -> Cell {
    boot_with(aicortex_api::Limits::default()).await
}

/// Boot the cell with `limits`.
pub async fn boot_with(limits: aicortex_api::Limits) -> Cell {
    let (store, dir) = open_store().await;
    let handle = store.handle();
    handle
        .migrate(aicortex_store::migrations())
        .await
        .expect("the schema applies");
    handle
        .migrate_sets(
            &[],
            &[rahi_store::coordination_set(), rahi_store::receipt_set()],
        )
        .await
        .expect("the work schema applies");
    let mut txn = TxnBuilder::new();
    ModelRegistry::activate(
        &mut txn,
        &ModelRevision {
            model_id: ModelId::new("test-local").expect("a model id"),
            revision: 1,
            dims: 3,
            normalized: true,
            first_seen: UnixSeconds::new(1_700_000_000),
            active: true,
        },
    )
    .expect("the model activates");
    handle
        .txn(txn.into_statements())
        .await
        .expect("the model row commits");

    let manifest = Manifest::parse(MANIFEST).expect("the test manifest parses");
    let ledger = Ledger::open(
        handle.clone(),
        LedgerSigner::from_seed([13_u8; 32]),
        manifest.hash().expect("the manifest hashes"),
    )
    .await
    .expect("the chain opens");
    let kernel = Kernel::boot(manifest, handle.clone(), ledger.clone())
        .await
        .expect("the kernel boots");

    // The key set stub: one route, the JWKS document.
    let (_, jwk) = signing_key();
    let keys = json!({ "keys": [jwk] });
    let stub = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a stub port");
    let stub_origin = format!("http://{}", stub.local_addr().expect("the stub port"));
    let stub_router = Router::new().route(
        "/jwks",
        get(move || {
            let keys = keys.clone();
            async move { axum::Json(keys) }
        }),
    );
    tokio::spawn(async move { axum::serve(stub, stub_router).await });

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("an app port");
    let origin = format!("http://{}", listener.local_addr().expect("the app port"));
    let env = BTreeMap::from([("RAHI_PUBLIC_URL", origin.as_str())]);
    let config = Config::from_env(&env).expect("the environment is well formed");
    let idp = IdpConfig::derive(&config, "aicortex").expect("the identity config derives");
    let resource = Resource::derive(&idp).expect("the resource derives");
    let issuer = resource.authorization_server().to_owned();
    let discovery = Discovery {
        issuer: issuer.clone(),
        authorization_endpoint: format!("{stub_origin}/authorize"),
        token_endpoint: format!("{stub_origin}/token"),
        userinfo_endpoint: format!("{stub_origin}/userinfo"),
        end_session_endpoint: format!("{stub_origin}/logout"),
        jwks_uri: format!("{stub_origin}/jwks"),
        revocation_endpoint: None,
    };
    let jwks = Jwks::load(&discovery).await.expect("the key set loads");
    let server = ResourceServer::new(&config, resource, jwks, handle.clone(), kernel.clone());

    let hub = StreamHub::new(StreamOptions {
        keep_alive: KEEP_ALIVE,
        ..StreamOptions::default()
    });
    let state =
        AppState::new(kernel.clone(), handle, ledger.clone(), config).with_extension(hub.clone());
    let api = aicortex_api::router_with(aicortex_api::Api::new(state.clone()).with_limits(limits));
    // The composition `serve` performs, minus sessions: the edge with the
    // bearer CSRF exemption of 038 B-1, then the bearer layer outside it.
    let edge = Edge::builder(state)
        .csrf_exemption(Arc::new(|path: &str, headers: &HeaderMap| {
            rahi_idp::is_bearer_route(path)
                && headers.contains_key(axum::http::header::AUTHORIZATION)
        }))
        .mount("/", api)
        .try_build()
        .map_err(|error| error.0)
        .expect("the edge builds");
    let app = with_bearer(
        RequireBearer::new(server, aicortex_api::bearer_routes()),
        edge,
    );
    tokio::spawn(async move { axum::serve(listener, app).await });

    Cell {
        store,
        ledger,
        kernel,
        hub,
        origin,
        // A test's loopback client to the cell it booted: not egress, which
        // 010 B-8's scan polices in the shipped sources.
        http: reqwest::Client::default(),
        issuer,
        dir,
    }
}

/// Who a token is for.
#[derive(Clone, Debug)]
pub struct Who {
    pub sub: String,
    pub client: String,
    pub scopes: Vec<&'static str>,
}

impl Who {
    /// A person, through the default client, holding `scopes`.
    pub fn person(sub: &str, scopes: &[&'static str]) -> Self {
        Self {
            sub: sub.to_owned(),
            client: CLIENT.to_owned(),
            scopes: scopes.to_vec(),
        }
    }

    /// A client acting as itself (client credentials, `rahi://025` B-4):
    /// rauthy maps `sub` to the client id.
    pub fn service(client: &str, scopes: &[&'static str]) -> Self {
        Self {
            sub: client.to_owned(),
            client: client.to_owned(),
            scopes: scopes.to_vec(),
        }
    }
}

impl Cell {
    /// A URL on the cell.
    pub fn url(&self, path: &str) -> String {
        format!("{}{path}", self.origin)
    }

    /// A signed access token for `who`, addressed to this resource.
    pub fn token(&self, who: &Who) -> String {
        let (pair, _) = signing_key();
        let now = now();
        let header = json!({ "alg": "RS256", "typ": "JWT", "kid": KID });
        let claims = json!({
            "iss": self.issuer,
            "aud": [who.client, self.origin],
            "sub": who.sub,
            "azp": who.client,
            "iat": now,
            "nbf": now,
            "exp": now + 600,
            "jti": format!("jti-{}", aicortex_types::MemoryId::now_v7()),
            "scope": who.scopes.join(" "),
        });
        let signing_input = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(header.to_string()),
            URL_SAFE_NO_PAD.encode(claims.to_string())
        );
        let mut signature = vec![0_u8; pair.public().modulus_len()];
        pair.sign(
            &RSA_PKCS1_SHA256,
            &SystemRandom::new(),
            signing_input.as_bytes(),
            &mut signature,
        )
        .expect("the token signs");
        format!("{signing_input}.{}", URL_SAFE_NO_PAD.encode(signature))
    }

    /// A request on the cell, carrying `who`'s token when there is one.
    pub fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        who: Option<&Who>,
    ) -> reqwest::RequestBuilder {
        let builder = self.http.request(method, self.url(path));
        match who {
            Some(who) => builder.bearer_auth(self.token(who)),
            None => builder,
        }
    }

    /// The records the chain holds, oldest first.
    pub async fn records(&self) -> Vec<SignedRecord> {
        self.ledger.records().await.expect("the chain reads")
    }

    /// How many rows `table` holds.
    pub async fn rows(&self, table: &str) -> u64 {
        #[derive(serde::Deserialize)]
        struct Count {
            count: i64,
        }
        let rows: Vec<Count> = self
            .store
            .handle()
            .query_consistent(format!("SELECT COUNT(*) AS count FROM {table}"), vec![])
            .await
            .expect("a count reads");
        u64::try_from(rows.first().map_or(0, |row| row.count)).expect("a non-negative count")
    }

    /// Stop the node.
    pub async fn shutdown(self) {
        drop(self.kernel);
        self.store.shutdown().await.expect("the node stops");
    }
}

/// A one-candidate capture body.
pub fn capture_body(text: &str) -> Value {
    json!({ "candidates": [{ "kind": "observation", "text": text }] })
}
