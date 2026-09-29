//! Local inference and verified model artifacts.

use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use rahi_types::Error;
use ring::digest::{Context, SHA256, digest};

use crate::provider::{EmbeddingProvider, ModelId, Vector, validate_batch};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

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
        if !self.url.starts_with("https://") {
            return Err(Error::Config(
                "model weights must be fetched over https".to_owned(),
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
        let safe_components = self.path.components().all(|component| {
            matches!(
                component,
                Component::Prefix(_) | Component::RootDir | Component::Normal(_)
            )
        });
        if !safe_components || !self.path.starts_with(models_dir) || self.path == models_dir {
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
        self.verify_path(&self.path)
    }

    fn verify_path(&self, path: &Path) -> Result<(), Error> {
        let mut file = std::fs::File::open(path).map_err(|error| {
            Error::Io(format!(
                "cannot read model artifact {}: {error}",
                path.display()
            ))
        })?;
        let mut context = Context::new(&SHA256);
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let read = file.read(&mut buffer).map_err(|error| {
                Error::Io(format!(
                    "cannot read model artifact {}: {error}",
                    path.display()
                ))
            })?;
            if read == 0 {
                break;
            }
            let chunk = buffer.get(..read).ok_or_else(|| {
                Error::Integrity("artifact read exceeded its fixed buffer".to_owned())
            })?;
            context.update(chunk);
        }
        let actual = hex(context.finish().as_ref());
        if actual != self.sha256 {
            return Err(Error::Integrity(format!(
                "model artifact {} has SHA-256 {actual}, expected {}",
                path.display(),
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
        let destination = confined_destination(&artifact.path, models_dir)?;
        if artifact.verify_path(&destination).is_ok() {
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
        let destination = confined_destination(&artifact.path, models_dir)?;
        let parent = destination.parent().ok_or_else(|| {
            Error::Config("canonical model artifact has no parent directory".to_owned())
        })?;
        let (temporary, mut file) = temporary_file(&destination)?;
        if let Err(error) = file.write_all(&bytes) {
            let _ = std::fs::remove_file(&temporary);
            return Err(Error::Io(format!(
                "cannot write model artifact {}: {error}",
                temporary.display()
            )));
        }
        if let Err(error) = file.sync_all() {
            let _ = std::fs::remove_file(&temporary);
            return Err(Error::Io(format!(
                "cannot sync model artifact {}: {error}",
                temporary.display()
            )));
        }
        drop(file);
        std::fs::rename(&temporary, &destination).map_err(|error| {
            let _ = std::fs::remove_file(&temporary);
            Error::Io(format!(
                "cannot install model artifact {}: {error}",
                destination.display()
            ))
        })?;
        std::fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| {
                Error::Io(format!(
                    "cannot sync model directory {}: {error}",
                    parent.display()
                ))
            })?;
        artifact.verify_path(&destination)
    }
}

fn confined_destination(path: &Path, models_dir: &Path) -> Result<PathBuf, Error> {
    if let Ok(metadata) = std::fs::symlink_metadata(models_dir)
        && metadata.file_type().is_symlink()
    {
        return Err(Error::Config(format!(
            "configured models directory {} must not be a symlink",
            models_dir.display()
        )));
    }
    std::fs::create_dir_all(models_dir).map_err(|error| {
        Error::Io(format!(
            "cannot create models directory {}: {error}",
            models_dir.display()
        ))
    })?;
    let parent = path.parent().ok_or_else(|| {
        Error::Config(format!(
            "model artifact {} has no parent directory",
            path.display()
        ))
    })?;
    let relative_parent = parent.strip_prefix(models_dir).map_err(|_| {
        Error::Config(format!(
            "model artifact {} is outside configured models directory {}",
            path.display(),
            models_dir.display()
        ))
    })?;
    let mut current = models_dir.to_path_buf();
    for component in relative_parent.components() {
        let Component::Normal(component) = component else {
            return Err(Error::Config(format!(
                "model artifact {} has an invalid directory component",
                path.display()
            )));
        };
        current.push(component);
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(Error::Config(format!(
                    "model directory {} must not be a symlink",
                    current.display()
                )));
            }
            Ok(metadata) if !metadata.is_dir() => {
                return Err(Error::Config(format!(
                    "model directory {} is not a directory",
                    current.display()
                )));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir(&current).map_err(|error| {
                    Error::Io(format!(
                        "cannot create model directory {}: {error}",
                        current.display()
                    ))
                })?;
            }
            Err(error) => {
                return Err(Error::Io(format!(
                    "cannot inspect model directory {}: {error}",
                    current.display()
                )));
            }
        }
    }
    if let Ok(metadata) = std::fs::symlink_metadata(path)
        && metadata.file_type().is_symlink()
    {
        return Err(Error::Config(format!(
            "model artifact {} must not be a symlink",
            path.display()
        )));
    }
    let canonical_root = std::fs::canonicalize(models_dir).map_err(|error| {
        Error::Io(format!(
            "cannot resolve models directory {}: {error}",
            models_dir.display()
        ))
    })?;
    let canonical_parent = std::fs::canonicalize(parent).map_err(|error| {
        Error::Io(format!(
            "cannot resolve model directory {}: {error}",
            parent.display()
        ))
    })?;
    if !canonical_parent.starts_with(&canonical_root) {
        return Err(Error::Config(format!(
            "model directory {} resolves outside configured models directory {}",
            parent.display(),
            models_dir.display()
        )));
    }
    let name = path.file_name().ok_or_else(|| {
        Error::Config(format!(
            "model artifact {} has no file name",
            path.display()
        ))
    })?;
    Ok(canonical_parent.join(name))
}

fn temporary_file(destination: &Path) -> Result<(PathBuf, std::fs::File), Error> {
    let name = destination
        .file_name()
        .ok_or_else(|| Error::Config("model artifact path has no file name".to_owned()))?;
    for _ in 0..16 {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let candidate = destination.with_file_name(format!(
            "{}.partial-{}-{sequence}",
            name.to_string_lossy(),
            std::process::id()
        ));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => return Ok((candidate, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(Error::Io(format!(
                    "cannot create model artifact {}: {error}",
                    candidate.display()
                )));
            }
        }
    }
    Err(Error::Io(format!(
        "cannot reserve a temporary file for model artifact {}",
        destination.display()
    )))
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

#[cfg(all(test, unix))]
#[allow(clippy::expect_used)]
mod tests {
    use std::os::unix::fs::symlink;

    use super::*;

    #[test]
    fn confined_destination_rejects_a_symlinked_subdirectory() {
        let root = tempfile::tempdir().expect("temporary root");
        let models = root.path().join("models");
        let outside = root.path().join("outside");
        std::fs::create_dir_all(&models).expect("models directory");
        std::fs::create_dir_all(&outside).expect("outside directory");
        symlink(&outside, models.join("linked")).expect("symlink fixture");

        let destination = models.join("linked/model.bin");
        assert!(confined_destination(&destination, &models).is_err());
        assert!(!outside.join("model.bin").exists());
    }
}
