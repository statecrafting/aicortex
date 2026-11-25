//! FR-009 (spec 020, 015 D-33): a capture through a booted `aicortex serve`,
//! embedded by the local provider, in a process that opens no outbound socket.
//!
//! The process boundary is the point. The test runs the real binary against a
//! real rauthy on loopback (`scripts/live-rauthy.sh up`, D-14) with the local
//! embedding provider selected and its pinned artifacts already in place, then
//!
//! - captures through `POST /api/v1/capture` with an access token rauthy
//!   issued after a PKCE login,
//! - waits for the embedding on `GET /api/v1/events` (the `embedding` event of
//!   B-6), which carries the memory id and a state and no body,
//! - samples the process's sockets for the whole window and asserts that every
//!   socket it holds, listening or connected, is loopback, and
//! - asserts the shipped manifest declares no egress host.
//!
//! Setup mirrors a deployment where rauthy is not the supervisor's child:
//! `first-boot` and `migrate` run as verbs, the fixture rauthy's administrator
//! API key is custodied as the cell's admin token, the cell's own OIDC client
//! is registered with `custody_client` (what `supervise` does after rauthy is
//! healthy), and the model is activated before `serve` with the function the
//! operator's `POST /operator/embedding/activate` calls, because that surface
//! needs a browser session and the cell's origin here is rauthy's (D-14).
//!
//! Without `AICORTEX_TEST_RAUTHY_URL` the test skips, and with
//! `AICORTEX_REQUIRE_RAUTHY=1` a skip is a failure.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

#[path = "../../../crates/aicortex-api/tests/common/live.rs"]
mod live;

use std::collections::BTreeMap;
use std::net::TcpListener;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aicortex::Aicortex;
use aicortex_embed::{EmbeddingConfig, operator};
use rahi_cli::Booted;
use rahi_kernel::Manifest;
use rahi_ops::KeySet;
use rahi_types::{Config, Sub};
use ring::digest::{SHA256, digest};
use safetensors::Dtype;
use safetensors::tensor::TensorView;
use serde_json::{Value, json};

const TOKENIZER: &str = r#"{
  "normalizer": {"type": "BertNormalizer", "lowercase": true},
  "pre_tokenizer": {"type": "BertPreTokenizer"},
  "model": {"type": "WordPiece", "unk_token": "[UNK]",
            "vocab": {"[UNK]": 0, "red": 1, "green": 2, "blue": 3}}
}"#;

const TEXT: &str = "red green blue memory embedded locally";

fn weights() -> Vec<u8> {
    let data: Vec<u8> = [9.0_f32, 9.0, 1.0, 0.0, 0.0, 1.0, 1.0, 1.0]
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect();
    let view = TensorView::new(Dtype::F32, vec![4, 2], &data).expect("view");
    safetensors::serialize(
        [(aicortex_embed::static_model::EMBEDDINGS_TENSOR, view)],
        None,
    )
    .expect("serialize")
}

fn sha(bytes: &[u8]) -> String {
    digest(&SHA256, bytes)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("a free port")
        .local_addr()
        .expect("its address")
        .port()
}

/// What the child holds, sampled with `lsof`: one `(name, state)` per socket.
fn sockets(pid: u32) -> Vec<String> {
    let output = Command::new("lsof")
        .args(["-nP", "-a", "-p", &pid.to_string(), "-i", "-F", "n"])
        .output()
        .expect("lsof runs (install it: the test needs a per-process socket view)");
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.strip_prefix('n'))
        .map(str::to_owned)
        .collect()
}

/// Whether a socket name (`127.0.0.1:1->127.0.0.1:2`, `*:8443`, `[::1]:5`)
/// touches only loopback or the wildcard a listener binds.
fn is_local(name: &str) -> bool {
    name.split("->").all(|end| {
        let host = end.rsplit_once(':').map_or(end, |(host, _)| host);
        matches!(host, "127.0.0.1" | "[::1]" | "*" | "localhost")
    })
}

fn run(verb: &str, env: &BTreeMap<String, String>) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_aicortex"))
        .arg(verb)
        .env_clear()
        .envs(env)
        .output()
        .unwrap_or_else(|err| panic!("aicortex {verb} did not run: {err}"))
}

struct Serving(Child);

impl Drop for Serving {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr009_a_capture_through_serve_embeds_locally_with_no_outbound_socket() {
    let Some(live) = live::LiveEnv::from_env("fr009") else {
        return;
    };

    // The shipped manifest declares no egress host.
    let manifest = Manifest::parse(Aicortex::MANIFEST).expect("the shipped manifest parses");
    assert!(
        manifest.resources.egress.is_empty(),
        "the shipped manifest declares an egress host"
    );

    let root = tempfile::tempdir().expect("a temporary root");
    let data = root.path().join("data");
    let models = root.path().join("models");
    std::fs::create_dir_all(&models).expect("models directory");
    let weights = weights();
    std::fs::write(models.join("model.safetensors"), &weights).expect("weights");
    std::fs::write(models.join("tokenizer.json"), TOKENIZER).expect("tokenizer");

    let app = free_port();
    let loopback = live.loopback_addr();
    let env: BTreeMap<String, String> = [
        ("RAHI_PUBLIC_URL", live.origin.clone()),
        ("RAHI_LISTEN_ADDR", format!("127.0.0.1:{app}")),
        ("RAHI_DATA_DIR", data.display().to_string()),
        (
            "RAHI_HIQLITE_API_ADDR",
            format!("127.0.0.1:{}", free_port()),
        ),
        (
            "RAHI_HIQLITE_RAFT_ADDR",
            format!("127.0.0.1:{}", free_port()),
        ),
        ("RAHI_RAUTHY_ADDR", loopback),
        ("RAHI_RAUTHY_MODE", "required".to_owned()),
        ("AICORTEX_EMBED_PROVIDER", "local".to_owned()),
        ("AICORTEX_EMBED_MODEL", "static-fr009".to_owned()),
        ("AICORTEX_EMBED_DIMS", "2".to_owned()),
        ("AICORTEX_EMBED_MODELS_DIR", models.display().to_string()),
        (
            "AICORTEX_EMBED_WEIGHTS_FILE",
            "model.safetensors".to_owned(),
        ),
        (
            "AICORTEX_EMBED_WEIGHTS_URL",
            "https://models.invalid/model.safetensors".to_owned(),
        ),
        ("AICORTEX_EMBED_WEIGHTS_SHA256", sha(&weights)),
        ("AICORTEX_EMBED_TOKENIZER_FILE", "tokenizer.json".to_owned()),
        (
            "AICORTEX_EMBED_TOKENIZER_URL",
            "https://models.invalid/tokenizer.json".to_owned(),
        ),
        ("AICORTEX_EMBED_TOKENIZER_SHA256", sha(TOKENIZER.as_bytes())),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_owned(), value))
    .collect();

    for verb in ["first-boot", "migrate"] {
        let output = run(verb, &env);
        assert_eq!(
            output.status.code(),
            Some(0),
            "{verb}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    // The fixture rauthy's administrator key is the cell's admin token, as
    // `supervise` would have written it for the rauthy it started.
    let token_path = data.join("keys").join("rauthy_admin_token");
    std::fs::write(&token_path, live.api_key.as_bytes()).expect("the admin token is custodied");

    // What `supervise` does once rauthy is healthy: register the cell's own
    // client and custody its secret.
    let config = Config::from_env(&env).expect("the environment is well formed");
    let keys = KeySet::of(&config);
    rahi_ops::supervise::custody_client(&config, &keys, &manifest)
        .await
        .expect("the cell's OIDC client is registered with the live rauthy");

    // Activation is an operator act (015 D-21); this is the function the
    // operator route calls, run against the deployment's own store.
    {
        let booted = Booted::open::<Aicortex>(&env)
            .await
            .expect("the deployment opens");
        let embedding = EmbeddingConfig::from_env(&env).expect("the embedding configuration");
        let activation = operator::activate_configured(
            &embedding,
            &manifest,
            None,
            &Sub::new("aicortex-operator"),
            aicortex_embed::NoTransport,
            &booted.store.handle(),
            aicortex_embed::unix_now(),
        )
        .await
        .expect("the local model activates");
        assert!(activation.changed);
        booted.store.shutdown().await.expect("the store closes");
    }

    let log = std::fs::File::create(root.path().join("serve.log")).expect("a log file");
    let child = Command::new(env!("CARGO_BIN_EXE_aicortex"))
        .arg("serve")
        .env_clear()
        .envs(&env)
        .stdout(Stdio::from(log.try_clone().expect("log clone")))
        .stderr(Stdio::from(log))
        .spawn()
        .expect("serve starts");
    let pid = child.id();
    let serving = Serving(child);
    let cell = format!("http://127.0.0.1:{app}");
    let http = reqwest::Client::default();

    let deadline = Instant::now() + Duration::from_secs(180);
    loop {
        if let Ok(answer) = http.get(format!("{cell}/readyz")).send().await
            && answer.status() == 200
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "serve did not become ready:\n{}",
            read_log(root.path())
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    // Sample the process's sockets from before the capture until after the
    // embedding has landed, so a connection opened while embedding is seen.
    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sampling = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let sampler = {
        let (seen, sampling) = (Arc::clone(&seen), Arc::clone(&sampling));
        std::thread::spawn(move || {
            while sampling.load(std::sync::atomic::Ordering::SeqCst) {
                seen.lock().unwrap().extend(sockets(pid));
                std::thread::sleep(Duration::from_millis(50));
            }
        })
    };

    let token = live::person_token(&live, &live.origin, &["memory.read", "memory.write"]).await;

    // Open the stream first so no event is missed.
    let mut stream = http
        .get(format!("{cell}/api/v1/events"))
        .bearer_auth(&token)
        .send()
        .await
        .expect("the stream opens");
    if stream.status() != 200 {
        let status = stream.status();
        panic!(
            "{status}: {}\n{}",
            stream.text().await.unwrap_or_default(),
            read_log(root.path())
        );
    }

    let captured = http
        .post(format!("{cell}/api/v1/capture"))
        .bearer_auth(&token)
        .header("content-type", "application/json")
        .body(json!({ "candidates": [{ "kind": "observation", "text": TEXT }] }).to_string())
        .send()
        .await
        .expect("the capture is answered");
    assert_eq!(captured.status(), 200);
    let body: Value = serde_json::from_str(&captured.text().await.unwrap()).unwrap();
    let id = body["results"][0]["id"]
        .as_str()
        .expect("an admitted id")
        .to_owned();

    // Wait for the embedding, then keep watching a little longer.
    let mut events = String::new();
    let deadline = Instant::now() + Duration::from_secs(90);
    while !(events.contains("event: embedding")
        && events.contains(&id)
        && events.contains("\"state\":\"embedded\""))
    {
        let chunk =
            tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), stream.chunk())
                .await
                .unwrap_or_else(|_| {
                    panic!(
                        "no embedding event for {id}:\n{events}\n{}",
                        read_log(root.path())
                    )
                })
                .expect("the stream reads")
                .expect("the stream stays open");
        events.push_str(&String::from_utf8_lossy(&chunk));
    }
    assert!(!events.contains(TEXT), "an event carried a memory body");
    tokio::time::sleep(Duration::from_secs(2)).await;

    sampling.store(false, std::sync::atomic::Ordering::SeqCst);
    sampler.join().expect("the sampler finishes");
    let observed = seen.lock().unwrap().clone();
    assert!(
        observed.iter().any(|name| name.contains(&app.to_string())),
        "the sampler saw the process's own listener, so it can see sockets: {observed:?}"
    );
    let foreign: Vec<_> = observed.iter().filter(|name| !is_local(name)).collect();
    assert!(
        foreign.is_empty(),
        "the process held a non-loopback socket: {foreign:?}"
    );
    eprintln!(
        "fr009: embedded {id}; {} socket samples, all loopback",
        observed.len()
    );
    drop(serving);
}

fn read_log(dir: &Path) -> String {
    std::fs::read_to_string(dir.join("serve.log")).unwrap_or_default()
}
