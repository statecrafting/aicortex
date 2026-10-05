//! Provider configuration, the manifest-ceiling check, and the local
//! provider's silence on the network (spec 015 B-4, B-5, B-6; FR-004, FR-005).

#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used
)]

use std::collections::BTreeMap;
use std::net::TcpListener;
use std::os::unix::fs::FileTypeExt;
use std::path::Path;

use aicortex_embed::provider::EmbeddingProvider;
use aicortex_embed::static_model::EMBEDDINGS_TENSOR;
use aicortex_embed::{EmbeddingConfig, NoTransport};
use rahi_kernel::Manifest;
use rahi_types::{Error, Sub};
use ring::digest::{SHA256, digest};
use safetensors::Dtype;
use safetensors::tensor::TensorView;

const APP_MANIFEST: &str = include_str!("../../../apps/aicortex/manifest.toml");

const TOKENIZER: &str = r#"{
  "normalizer": {"type": "BertNormalizer", "lowercase": true},
  "pre_tokenizer": {"type": "BertPreTokenizer"},
  "model": {"type": "WordPiece", "unk_token": "[UNK]",
            "vocab": {"[UNK]": 0, "red": 1, "green": 2}}
}"#;

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

fn granted_manifest(host: &str) -> Manifest {
    Manifest::parse(&format!(
        r#"
schema_version = "1.0.0"

[app]
name = "aicortex"
org = "statecrafting"

[resources]
egress = ["{host}"]

[[capabilities]]
id = "embedding-egress"
kind = "http.egress"
resource = "{host}"

[services.embedding]
capabilities = ["embedding-egress"]

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

fn app_manifest() -> Manifest {
    Manifest::parse(APP_MANIFEST).expect("the shipped manifest parses")
}

fn remote_env(endpoint: &str) -> BTreeMap<String, String> {
    [
        ("AICORTEX_EMBED_PROVIDER", "remote"),
        ("AICORTEX_EMBED_MODEL", "remote-model"),
        ("AICORTEX_EMBED_DIMS", "8"),
        ("AICORTEX_EMBED_ENDPOINT", endpoint),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_owned(), value.to_owned()))
    .collect()
}

fn local_env(models: &Path, weights_sha: &str, tokenizer_sha: &str) -> BTreeMap<String, String> {
    [
        ("AICORTEX_EMBED_MODEL", "static-fixture"),
        ("AICORTEX_EMBED_DIMS", "2"),
        ("AICORTEX_EMBED_MODELS_DIR", models.to_str().expect("utf-8")),
        ("AICORTEX_EMBED_WEIGHTS_FILE", "model.safetensors"),
        (
            "AICORTEX_EMBED_WEIGHTS_URL",
            "https://models.example/m.safetensors",
        ),
        ("AICORTEX_EMBED_WEIGHTS_SHA256", weights_sha),
        ("AICORTEX_EMBED_TOKENIZER_FILE", "tokenizer.json"),
        (
            "AICORTEX_EMBED_TOKENIZER_URL",
            "https://models.example/tokenizer.json",
        ),
        ("AICORTEX_EMBED_TOKENIZER_SHA256", tokenizer_sha),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_owned(), value.to_owned()))
    .collect()
}

#[test]
fn an_unconfigured_deployment_has_no_provider() {
    let empty: BTreeMap<String, String> = BTreeMap::new();
    assert_eq!(
        EmbeddingConfig::from_env(&empty).expect("parses"),
        EmbeddingConfig::Disabled
    );
    let off: BTreeMap<String, String> =
        [("AICORTEX_EMBED_PROVIDER".to_owned(), "off".to_owned())].into();
    assert_eq!(
        EmbeddingConfig::from_env(&off).expect("parses"),
        EmbeddingConfig::Disabled
    );
}

#[test]
fn malformed_configuration_names_the_variable() {
    let mut env = remote_env("https://models.example/embed");
    env.remove("AICORTEX_EMBED_MODEL");
    let error = EmbeddingConfig::from_env(&env).expect_err("provider without model");
    assert!(
        error.to_string().contains("AICORTEX_EMBED_MODEL"),
        "{error}"
    );

    let mut env = remote_env("https://models.example/embed");
    env.insert("AICORTEX_EMBED_DIMS".to_owned(), "wide".to_owned());
    assert!(
        EmbeddingConfig::from_env(&env)
            .expect_err("bad dims")
            .to_string()
            .contains("AICORTEX_EMBED_DIMS")
    );

    let mut env = remote_env("https://models.example/embed");
    env.insert("AICORTEX_EMBED_PROVIDER".to_owned(), "cloud".to_owned());
    assert!(EmbeddingConfig::from_env(&env).is_err());

    let root = tempfile::tempdir().expect("temporary root");
    let mut env = local_env(root.path(), &"0".repeat(64), &"0".repeat(64));
    env.insert(
        "AICORTEX_EMBED_WEIGHTS_FILE".to_owned(),
        "../escape.safetensors".to_owned(),
    );
    assert!(EmbeddingConfig::from_env(&env).is_err());
}

/// FR-005: a remote provider whose host the ceiling does not admit fails the
/// check with a named capability error, before anything is booted.
#[test]
fn a_remote_host_absent_from_the_manifest_fails_with_a_named_capability_error() {
    let config =
        EmbeddingConfig::from_env(&remote_env("https://models.example/v1/embed")).expect("parses");

    let denied = config
        .check(&app_manifest())
        .expect_err("the shipped manifest declares no egress");
    assert!(matches!(denied, Error::Denied(_)), "{denied}");
    let text = denied.to_string();
    assert!(text.contains("embedding.remote.egress"), "{text}");
    assert!(text.contains("models.example"), "{text}");
    let failure = config
        .check_ceiling(&app_manifest())
        .expect_err("the structured form names the same failure");
    assert_eq!(failure.check, "embedding.remote.egress");
    assert_eq!(failure.service, "embedding");
    assert_eq!(failure.host, "models.example");

    // A different host in the ceiling does not admit this one.
    assert!(config.check(&granted_manifest("other.example")).is_err());
    // Adding the host (the reviewable manifest diff) admits it.
    config
        .check(&granted_manifest("models.example"))
        .expect("the declared host is admitted");
    // A wildcard in the ceiling covers a concrete host.
    config
        .check(&granted_manifest("*.example"))
        .expect("a wildcard covers a subdomain");
}

#[test]
fn a_remote_endpoint_that_is_not_https_is_a_configuration_error() {
    let config = EmbeddingConfig::from_env(&remote_env("http://models.example/embed"))
        .expect("the shape parses");
    assert!(matches!(
        config.check(&granted_manifest("models.example")),
        Err(Error::Config(_))
    ));
}

#[test]
fn local_artifacts_need_no_egress_while_present_and_name_the_fetch_when_absent() {
    let root = tempfile::tempdir().expect("temporary root");
    let models = root.path().join("models");
    std::fs::create_dir_all(&models).expect("models directory");
    let env = local_env(&models, &sha(&weights()), &sha(TOKENIZER.as_bytes()));
    let config = EmbeddingConfig::from_env(&env).expect("parses");

    // Absent artifacts would need a fetch, which the ceiling does not allow.
    let failure = config
        .check_ceiling(&app_manifest())
        .expect_err("a fetch needs egress");
    assert_eq!(failure.check, "embedding.weights.egress");

    std::fs::write(models.join("model.safetensors"), weights()).expect("weights");
    std::fs::write(models.join("tokenizer.json"), TOKENIZER).expect("tokenizer");
    config
        .check(&app_manifest())
        .expect("present, verified artifacts need no egress");
}

fn socket_count() -> usize {
    std::fs::read_dir("/dev/fd")
        .expect("fd directory")
        .filter_map(Result::ok)
        .filter(|entry| {
            std::fs::metadata(entry.path()).is_ok_and(|meta| meta.file_type().is_socket())
        })
        .count()
}

/// FR-004: with the local provider selected the shipped manifest declares no
/// egress host, and booting the provider and embedding opens no socket.
///
/// The probe brackets the embedding path (boot, verify, inference). The
/// store's own loopback sockets belong to the chassis and are outside it. A
/// whole-process probe over a booted `serve` waits on the managed-service
/// lifecycle, so the requirement stays open in the spec Status.
#[tokio::test]
async fn the_local_provider_opens_no_socket_and_the_manifest_declares_no_egress() {
    let manifest = app_manifest();
    assert!(manifest.resources.egress.is_empty());
    assert!(manifest.capabilities.is_empty());

    // The probe can see a socket when one exists.
    let before = socket_count();
    let listener = TcpListener::bind("127.0.0.1:0").expect("control socket");
    assert!(
        socket_count() > before,
        "the probe detects a control socket"
    );
    drop(listener);

    let root = tempfile::tempdir().expect("temporary root");
    let models = root.path().join("models");
    std::fs::create_dir_all(&models).expect("models directory");
    std::fs::write(models.join("model.safetensors"), weights()).expect("weights");
    std::fs::write(models.join("tokenizer.json"), TOKENIZER).expect("tokenizer");
    let env = local_env(&models, &sha(&weights()), &sha(TOKENIZER.as_bytes()));
    let config = EmbeddingConfig::from_env(&env).expect("parses");

    let before = socket_count();
    let provider = config
        .boot(None, &Sub::new("probe"), NoTransport)
        .await
        .expect("local provider boots without egress")
        .expect("a provider is configured");
    let vectors = provider.embed(&["red green"]).await.expect("embeds");
    assert_eq!(vectors.len(), 1);
    assert_eq!(socket_count(), before, "no socket was opened");
}
