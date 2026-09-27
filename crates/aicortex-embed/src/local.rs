//! Local inference and verified model artifacts.

use std::path::{Path, PathBuf};

use rahi_types::Error;
use ring::digest::{SHA256, digest};

use crate::provider::{EmbeddingProvider, ModelId, Vector, validate_batch};

/// The configured model artifact fetched by preflight, or supplied by the
/// image.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WeightArtifact {
    /// Destination under the deployment's `models/` directory.
    pub path: PathBuf,
    /// Source used only by the preflight fetcher when the artifact is absent.
    pub url: String,
    /// Lowercase SHA-256 of the expected bytes.
    pub sha256: String,
}

impl WeightArtifact {
    /// Validate configuration before any file or network operation.
    ///
    /// # Errors
    ///
    /// [`Error::Config`] for a path outside `models_dir`, an empty URL, or an
    /// invalid digest.
    pub fn validate(&self, models_dir: &Path) -> Result<(), Error> {
        if self.url.is_empty() {
            return Err(Error::Config(
                "model weights need a configured URL".to_owned(),
            ));
        }
        if self.sha256.len() != 64
            || !self
                .sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        {
            return Err(Error::Config(
                "model weights need a 64-character lowercase SHA-256".to_owned(),
            ));
        }
        if !self.path.starts_with(models_dir) {
            return Err(Error::Config(format!(
                "model artifact {} is outside configured models directory {}",
                self.path.display(),
                models_dir.display()
            )));
        }
        Ok(())
    }

    /// Verify an existing artifact without making an outbound call.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] when the file cannot be read; [`Error::Integrity`] when
    /// its digest differs from the pin.
    pub fn verify(&self) -> Result<(), Error> {
        let bytes = std::fs::read(&self.path).map_err(|error| {
            Error::Io(format!(
                "cannot read model artifact {}: {error}",
                self.path.display()
            ))
        })?;
        let actual = hex(digest(&SHA256, &bytes).as_ref());
        if actual != self.sha256 {
            return Err(Error::Integrity(format!(
                "model artifact {} has SHA-256 {actual}, expected {}",
                self.path.display(),
                self.sha256
            )));
        }
        Ok(())
    }
}

/// A preflight-owned transport for the one-time artifact fetch.
#[allow(async_fn_in_trait)]
pub trait WeightFetcher: Send + Sync {
    /// Fetch `url` under deployment governance and return its bytes.
    async fn fetch(&self, url: &str) -> Result<Vec<u8>, Error>;
}

/// The configured sentence model implementation.
///
/// Model selection stays configuration. This trait keeps the provider free
/// of an embedded HTTP client and makes runtime inference an in-process call.
#[allow(async_fn_in_trait)]
pub trait LocalEngine: Send + Sync {
    /// Run CPU inference over the already loaded artifact.
    async fn infer(&self, weights: &Path, batch: &[&str]) -> Result<Vec<Vector>, Error>;
}

/// The local default. Calling [`EmbeddingProvider::embed`] performs no file
/// download and no network operation.
#[derive(Clone, Debug)]
pub struct LocalProvider<E> {
    id: ModelId,
    dims: u16,
    normalized: bool,
    weights: WeightArtifact,
    engine: E,
}

impl<E> LocalProvider<E> {
    /// Construct a provider after preflight has verified the artifact.
    ///
    /// # Errors
    ///
    /// The artifact verification error, or [`Error::Config`] for zero dims.
    pub fn boot(
        id: ModelId,
        dims: u16,
        normalized: bool,
        weights: WeightArtifact,
        engine: E,
    ) -> Result<Self, Error> {
        if dims == 0 {
            return Err(Error::Config(
                "an embedding model must declare at least one dimension".to_owned(),
            ));
        }
        weights.verify()?;
        Ok(Self {
            id,
            dims,
            normalized,
            weights,
            engine,
        })
    }

    /// Ensure the artifact exists and matches its pin.
    ///
    /// Existing valid bytes cause no fetch. Missing or invalid bytes are
    /// fetched through the caller's governed preflight transport, verified
    /// before rename, and never exposed under the final name unverified.
    ///
    /// # Errors
    ///
    /// Configuration, fetch, filesystem, or digest verification errors.
    pub async fn ensure_weights(
        artifact: &WeightArtifact,
        models_dir: &Path,
        fetcher: &impl WeightFetcher,
    ) -> Result<(), Error> {
        artifact.validate(models_dir)?;
        if artifact.verify().is_ok() {
            return Ok(());
        }
        let bytes = fetcher.fetch(&artifact.url).await?;
        let actual = hex(digest(&SHA256, &bytes).as_ref());
        if actual != artifact.sha256 {
            return Err(Error::Integrity(format!(
                "downloaded model artifact has SHA-256 {actual}, expected {}",
                artifact.sha256
            )));
        }
        let parent = artifact.path.parent().ok_or_else(|| {
            Error::Config(format!(
                "model artifact {} has no parent directory",
                artifact.path.display()
            ))
        })?;
        std::fs::create_dir_all(parent).map_err(|error| {
            Error::Io(format!(
                "cannot create model directory {}: {error}",
                parent.display()
            ))
        })?;
        let temporary = artifact.path.with_extension("partial");
        std::fs::write(&temporary, bytes).map_err(|error| {
            Error::Io(format!(
                "cannot write model artifact {}: {error}",
                temporary.display()
            ))
        })?;
        std::fs::rename(&temporary, &artifact.path).map_err(|error| {
            Error::Io(format!(
                "cannot install model artifact {}: {error}",
                artifact.path.display()
            ))
        })?;
        artifact.verify()
    }
}

impl<E: LocalEngine> EmbeddingProvider for LocalProvider<E> {
    fn id(&self) -> ModelId {
        self.id.clone()
    }

    fn dims(&self) -> u16 {
        self.dims
    }

    fn normalized(&self) -> bool {
        self.normalized
    }

    async fn embed(&self, batch: &[&str]) -> Result<Vec<Vector>, Error> {
        let vectors = self.engine.infer(&self.weights.path, batch).await?;
        validate_batch(self, batch.len(), &vectors)?;
        Ok(vectors)
    }
}

fn hex(bytes: &[u8]) -> String {
    use core::fmt::Write as _;
    let mut value = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        let _ = write!(value, "{byte:02x}");
    }
    value
}
