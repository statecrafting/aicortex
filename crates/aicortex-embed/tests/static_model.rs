//! The static-embedding engine behind `LocalProvider` (spec 015 B-5, D-22),
//! held to a hand-computed fixture. No network, no real weights.

#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used
)]

use std::collections::BTreeMap;

use aicortex_embed::provider::{EmbeddingProvider, ModelId};
use aicortex_embed::static_model::EMBEDDINGS_TENSOR;
use aicortex_embed::{LocalProvider, StaticEmbeddingEngine, WeightArtifact};
use ndarray::Array1;
use ring::digest::{SHA256, digest};
use safetensors::Dtype;
use safetensors::tensor::TensorView;

const TOKENIZER: &str = r#"{
  "normalizer": {"type": "BertNormalizer", "lowercase": true},
  "pre_tokenizer": {"type": "BertPreTokenizer"},
  "model": {"type": "WordPiece", "unk_token": "[UNK]",
            "vocab": {"[UNK]": 0, "red": 1, "green": 2, "blue": 3}}
}"#;

/// Rows: unk, red, green, blue.
const TABLE: [[f32; 2]; 4] = [[9.0, 9.0], [1.0, 0.0], [0.0, 1.0], [1.0, 1.0]];

fn weights(dtype: Dtype) -> Vec<u8> {
    let mut data = Vec::new();
    for value in TABLE.iter().flatten() {
        match dtype {
            Dtype::F16 => data.extend_from_slice(&half::f16::from_f32(*value).to_le_bytes()),
            _ => data.extend_from_slice(&value.to_le_bytes()),
        }
    }
    let view = TensorView::new(dtype, vec![4, 2], &data).expect("view");
    safetensors::serialize([(EMBEDDINGS_TENSOR, view)], None).expect("serialize")
}

fn engine(normalize: bool) -> StaticEmbeddingEngine {
    StaticEmbeddingEngine::new(TOKENIZER.as_bytes(), normalize, 16).expect("engine")
}

fn values(engine: &StaticEmbeddingEngine, bytes: &[u8], text: &str) -> Vec<f32> {
    engine
        .embed_batch(bytes, &[text])
        .expect("embeds")
        .remove(0)
        .values()
        .to_vec()
}

fn close(left: &[f32], right: &[f32]) -> bool {
    left.len() == right.len()
        && (Array1::from_vec(left.to_vec()) - Array1::from_vec(right.to_vec()))
            .iter()
            .all(|difference| difference.abs() < 1e-6)
}

#[test]
fn the_sentence_vector_is_the_mean_of_its_token_rows() {
    let engine = engine(false);
    let bytes = weights(Dtype::F32);
    assert!(close(&values(&engine, &bytes, "Red green"), &[0.5, 0.5]));
    assert!(close(&values(&engine, &bytes, "blue"), &[1.0, 1.0]));
}

#[test]
fn normalization_is_unit_length_and_f16_tables_decode() {
    let engine = engine(true);
    for dtype in [Dtype::F32, Dtype::F16] {
        let bytes = weights(dtype);
        let half = std::f32::consts::FRAC_1_SQRT_2;
        assert!(close(&values(&engine, &bytes, "blue"), &[half, half]));
        assert!(close(&values(&engine, &bytes, "red"), &[1.0, 0.0]));
    }
}

#[test]
fn unknown_tokens_do_not_pollute_the_mean_and_empty_text_is_refused() {
    let engine = engine(false);
    let bytes = weights(Dtype::F32);
    assert!(close(&values(&engine, &bytes, "red zzz"), &[1.0, 0.0]));
    assert!(engine.embed_batch(&bytes, &["zzz"]).is_err());
    assert!(engine.embed_batch(&bytes, &[""]).is_err());
}

#[test]
fn a_table_row_with_infinity_is_refused_not_returned_as_nan() {
    let engine = engine(true);
    let data: Vec<u8> = [9.0_f32, 9.0, f32::INFINITY, 0.0, 0.0, 1.0, 1.0, 1.0]
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect();
    let view = TensorView::new(Dtype::F32, vec![4, 2], &data).expect("view");
    let bytes = safetensors::serialize([(EMBEDDINGS_TENSOR, view)], None).expect("serialize");
    let error = engine.embed_batch(&bytes, &["red"]).expect_err("refused");
    assert!(error.to_string().contains("magnitude"), "{error}");
}

#[test]
fn unsupported_or_inconsistent_weights_are_refused() {
    let engine = engine(false);
    let data = vec![0_u8; 8];
    let view = TensorView::new(Dtype::F32, vec![1, 2], &data).expect("view");
    let extra = TensorView::new(Dtype::F32, vec![1, 2], &data).expect("view");
    let tensors: BTreeMap<_, _> = [(EMBEDDINGS_TENSOR, view), ("mapping", extra)].into();
    let layered = safetensors::serialize(tensors, None).expect("serialize");
    assert!(engine.embed_batch(&layered, &["red"]).is_err());

    // The tokenizer names id 3, so a two-row table is a mismatch.
    let short = TensorView::new(Dtype::F32, vec![1, 2], &data).expect("view");
    let short = safetensors::serialize([(EMBEDDINGS_TENSOR, short)], None).expect("serialize");
    assert!(engine.embed_batch(&short, &["red"]).is_err());
    assert!(engine.embed_batch(b"not safetensors", &["red"]).is_err());
}

fn sha(bytes: &[u8]) -> String {
    digest(&SHA256, bytes)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[tokio::test]
async fn local_provider_serves_the_engine_from_digest_pinned_artifacts() {
    let root = tempfile::tempdir().expect("temporary root");
    let models = root.path().join("models");
    std::fs::create_dir_all(&models).expect("models directory");
    let bytes = weights(Dtype::F32);
    std::fs::write(models.join("model.safetensors"), &bytes).expect("weights");
    std::fs::write(models.join("tokenizer.json"), TOKENIZER).expect("tokenizer");
    let artifact = |name: &str, content: &[u8]| WeightArtifact {
        path: models.join(name),
        url: format!("https://models.example/{name}"),
        sha256: sha(content),
    };
    let tokenizer = artifact("tokenizer.json", TOKENIZER.as_bytes());
    let engine = StaticEmbeddingEngine::from_artifact(&tokenizer, &models, true, 16)
        .expect("engine from verified tokenizer");
    let provider = LocalProvider::boot(
        ModelId::new("static-fixture").expect("model id"),
        2,
        engine.normalizes(),
        artifact("model.safetensors", &bytes),
        &models,
        engine,
    )
    .expect("provider boots");
    let vectors = provider
        .embed(&["red", "green blue"])
        .await
        .expect("embeds");
    assert_eq!(vectors.len(), 2);
    assert!(close(vectors[0].values(), &[1.0, 0.0]));

    // A tampered tokenizer is refused before it is parsed.
    std::fs::write(models.join("tokenizer.json"), b"{}").expect("tamper");
    assert!(StaticEmbeddingEngine::from_artifact(&tokenizer, &models, true, 16).is_err());
}

/// Runs the real model named by `AICORTEX_TEST_MODEL_DIR`, a directory holding
/// `model.safetensors` and `tokenizer.json` of a Model2Vec distillation. Not
/// run by default: it needs weights the repository does not carry.
#[test]
#[ignore = "needs real weights: set AICORTEX_TEST_MODEL_DIR"]
fn a_real_model_orders_related_sentences_above_unrelated_ones() {
    let dir = std::env::var("AICORTEX_TEST_MODEL_DIR").expect("AICORTEX_TEST_MODEL_DIR");
    let dir = std::path::Path::new(&dir);
    let tokenizer = std::fs::read(dir.join("tokenizer.json")).expect("tokenizer.json");
    let weights = std::fs::read(dir.join("model.safetensors")).expect("model.safetensors");
    let engine = StaticEmbeddingEngine::new(&tokenizer, true, 512).expect("engine");
    let vectors = engine
        .embed_batch(
            &weights,
            &[
                "The cat sat on the warm windowsill.",
                "A kitten rested on the sunny ledge.",
                "Quarterly revenue grew eight percent.",
            ],
        )
        .expect("embeds");
    let dot =
        |a: &[f32], b: &[f32]| Array1::from_vec(a.to_vec()).dot(&Array1::from_vec(b.to_vec()));
    assert!(
        dot(vectors[0].values(), vectors[1].values())
            > dot(vectors[0].values(), vectors[2].values())
    );
}
