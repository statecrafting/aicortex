//! The in-process CPU engine for `LocalProvider` (spec 015 B-5, D-22).
//!
//! The engine runs a static sentence-embedding model: a WordPiece tokenizer
//! and a token embedding table whose mean is the sentence vector, optionally
//! L2-normalized. This is the family of Model2Vec distillations. Inference is
//! a table lookup and a mean, so it needs no accelerator, no native library,
//! and no network, and a 256-dimension model of several million parameters
//! embeds a chunk in microseconds.
//!
//! The weight artifact is a safetensors file with one tensor named
//! `embeddings` of shape `[vocabulary, dims]` in `F32`, `F16`, or `BF16`. A
//! file carrying the optional `weights` or `mapping` tensors of the newer
//! Model2Vec layout is refused: those change the arithmetic, and ignoring
//! them would return vectors the model's authors never defined.
//!
//! All floating-point arithmetic is `ndarray`'s. This module contains no
//! float operator, so the workspace `float_arithmetic` ratchet needs no
//! exception here.

use std::path::Path;

use ndarray::Array1;
use rahi_types::Error;
use safetensors::{Dtype, SafeTensors};

use crate::local::{LocalEngine, WeightArtifact};
use crate::provider::Vector;
use crate::wordpiece::WordPiece;

/// The tensor holding the token embedding table.
pub const EMBEDDINGS_TENSOR: &str = "embeddings";

/// Default token budget per text, matching the model family's own default.
pub const DEFAULT_MAX_TOKENS: usize = 512;

/// A static embedding model's tokenizer and pooling rule.
#[derive(Clone, Debug)]
pub struct StaticEmbeddingEngine {
    tokenizer: WordPiece,
    normalize: bool,
    max_tokens: usize,
}

impl StaticEmbeddingEngine {
    /// Build the engine from a `tokenizer.json` document.
    ///
    /// # Errors
    ///
    /// [`Error::Config`] for an unsupported tokenizer or a token budget
    /// outside `1..=65535`.
    pub fn new(tokenizer_json: &[u8], normalize: bool, max_tokens: usize) -> Result<Self, Error> {
        if max_tokens == 0 || u16::try_from(max_tokens).is_err() {
            return Err(Error::Config(
                "the embedding token budget must be in 1..=65535".to_owned(),
            ));
        }
        Ok(Self {
            tokenizer: WordPiece::from_json(tokenizer_json)?,
            normalize,
            max_tokens,
        })
    }

    /// Build the engine from a pinned tokenizer artifact under `models_dir`.
    ///
    /// The artifact is confined to the models directory and verified against
    /// its digest before it is parsed.
    ///
    /// # Errors
    ///
    /// Artifact verification errors, or those of [`Self::new`].
    pub fn from_artifact(
        tokenizer: &WeightArtifact,
        models_dir: &Path,
        normalize: bool,
        max_tokens: usize,
    ) -> Result<Self, Error> {
        let bytes = tokenizer.read_verified(models_dir)?;
        Self::new(&bytes, normalize, max_tokens)
    }

    /// Whether this engine returns L2-normalized vectors.
    #[must_use]
    pub const fn normalizes(&self) -> bool {
        self.normalize
    }

    /// Embed a batch against the given safetensors weights.
    ///
    /// # Errors
    ///
    /// [`Error::Config`] for a weight file this engine does not support,
    /// [`Error::Integrity`] for a table that disagrees with the tokenizer,
    /// and [`Error::Validation`] for a text with no embeddable token.
    pub fn embed_batch(&self, weights: &[u8], batch: &[&str]) -> Result<Vec<Vector>, Error> {
        let tensors = SafeTensors::deserialize(weights).map_err(|error| {
            Error::Config(format!("model weights are not safetensors: {error}"))
        })?;
        if tensors
            .names()
            .iter()
            .any(|name| *name != EMBEDDINGS_TENSOR)
        {
            return Err(Error::Config(
                "model weights carry tensors besides `embeddings`, a layout this engine does not \
                 support"
                    .to_owned(),
            ));
        }
        let table = tensors.tensor(EMBEDDINGS_TENSOR).map_err(|error| {
            Error::Config(format!(
                "model weights have no `embeddings` tensor: {error}"
            ))
        })?;
        let table = Table::new(table.dtype(), table.shape(), table.data())?;
        if self.tokenizer.id_bound() > table.rows {
            return Err(Error::Integrity(format!(
                "the tokenizer names ids up to {} but the embedding table has {} rows",
                self.tokenizer.id_bound(),
                table.rows
            )));
        }
        batch
            .iter()
            .map(|text| self.embed_one(&table, text))
            .collect()
    }

    fn embed_one(&self, table: &Table<'_>, text: &str) -> Result<Vector, Error> {
        let unknown = self.tokenizer.unknown_id();
        let ids: Vec<u32> = self
            .tokenizer
            .encode(text, self.max_tokens)
            .into_iter()
            .filter(|id| *id != unknown)
            .collect();
        let count = u16::try_from(ids.len())
            .map_err(|_| Error::Integrity("token count exceeds the budget".to_owned()))?;
        if count == 0 {
            return Err(Error::Validation(
                "the text has no token this model can embed".to_owned(),
            ));
        }
        let mut sum = Array1::<f32>::zeros(table.dims);
        for id in ids {
            sum += &table.row(id)?;
        }
        let mut pooled = sum / f32::from(count);
        if self.normalize {
            let norm = pooled.dot(&pooled).sqrt();
            if norm.is_nan() || norm <= f32::MIN_POSITIVE {
                return Err(Error::Validation(
                    "the pooled vector has no magnitude to normalize".to_owned(),
                ));
            }
            pooled /= norm;
        }
        Vector::new(pooled.to_vec())
    }
}

impl LocalEngine for StaticEmbeddingEngine {
    async fn infer(&self, weights: &[u8], batch: &[&str]) -> Result<Vec<Vector>, Error> {
        self.embed_batch(weights, batch)
    }
}

struct Table<'a> {
    data: &'a [u8],
    dtype: Dtype,
    rows: u64,
    dims: usize,
}

impl<'a> Table<'a> {
    fn new(dtype: Dtype, shape: &[usize], data: &'a [u8]) -> Result<Self, Error> {
        if !matches!(dtype, Dtype::F32 | Dtype::F16 | Dtype::BF16) {
            return Err(Error::Config(format!(
                "embedding table dtype {dtype:?} is unsupported; use F32, F16, or BF16"
            )));
        }
        let [rows, dims] = shape else {
            return Err(Error::Config(
                "the embedding table must have shape [vocabulary, dims]".to_owned(),
            ));
        };
        if *dims == 0 || u16::try_from(*dims).is_err() {
            return Err(Error::Config(format!(
                "embedding table width {dims} is outside 1..=65535"
            )));
        }
        let table = Self {
            data,
            dtype,
            rows: u64::try_from(*rows).unwrap_or(u64::MAX),
            dims: *dims,
        };
        let expected = rows
            .checked_mul(*dims)
            .and_then(|cells| cells.checked_mul(table.width()))
            .ok_or_else(|| Error::Integrity("embedding table size overflows".to_owned()))?;
        if data.len() != expected {
            return Err(Error::Integrity(format!(
                "embedding table has {} bytes, expected {expected}",
                data.len()
            )));
        }
        Ok(table)
    }

    const fn width(&self) -> usize {
        match self.dtype {
            Dtype::F32 => 4,
            _ => 2,
        }
    }

    fn row(&self, id: u32) -> Result<Array1<f32>, Error> {
        let row_bytes = self.dims.saturating_mul(self.width());
        let start = usize::try_from(id)
            .ok()
            .and_then(|id| id.checked_mul(row_bytes))
            .ok_or_else(|| Error::Integrity("token id is outside the table".to_owned()))?;
        let bytes = start
            .checked_add(row_bytes)
            .and_then(|end| self.data.get(start..end))
            .ok_or_else(|| Error::Integrity(format!("token id {id} is outside the table")))?;
        let values = match self.dtype {
            Dtype::F32 => bytes
                .chunks_exact(4)
                .map(|cell| {
                    <[u8; 4]>::try_from(cell)
                        .map(f32::from_le_bytes)
                        .map_err(|_| Error::Integrity("table cell is not four bytes".to_owned()))
                })
                .collect::<Result<Vec<_>, _>>()?,
            dtype => bytes
                .chunks_exact(2)
                .map(|cell| {
                    let raw = <[u8; 2]>::try_from(cell)
                        .map_err(|_| Error::Integrity("table cell is not two bytes".to_owned()))?;
                    Ok(if dtype == Dtype::F16 {
                        half::f16::from_le_bytes(raw).to_f32()
                    } else {
                        half::bf16::from_le_bytes(raw).to_f32()
                    })
                })
                .collect::<Result<Vec<_>, Error>>()?,
        };
        Ok(Array1::from_vec(values))
    }
}
