//! Local inference and verified model artifacts.

use std::any::type_name;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use rahi_kernel::{Egress, Governed, Permit};
use rahi_types::{Error, Sub};
use ring::digest::{Context, SHA256, digest};

use crate::provider::{EmbeddingProvider, ModelId, Vector, validate_batch};
use crate::remote::parse_https_endpoint;

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Maximum accepted size of one fetched model artifact.
pub const MAX_WEIGHT_BYTES: usize = 1024 * 1024 * 1024;

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
        let relative_path = self.path.strip_prefix(models_dir).map_err(|_| {
            Error::Config(format!(
                "model artifact {} is outside configured models directory {}",
                self.path.display(),
                models_dir.display()
            ))
        })?;
        let safe_components = relative_path
            .components()
            .all(|component| matches!(component, Component::Normal(_)));
        if relative_path.as_os_str().is_empty() || !safe_components {
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
        let mut file = self.open_bounded(path)?;
        let mut context = Context::new(&SHA256);
        let mut total = 0_usize;
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let read = read_artifact(&mut file, path, &mut buffer)?;
            if read == 0 {
                break;
            }
            let chunk = buffer.get(..read).ok_or_else(|| {
                Error::Integrity("artifact read exceeded its fixed buffer".to_owned())
            })?;
            total = total.checked_add(read).ok_or_else(|| too_large(path))?;
            if total > MAX_WEIGHT_BYTES {
                return Err(too_large(path));
            }
            context.update(chunk);
        }
        self.check_digest(path, context)
    }

    fn read_verified_path(&self, path: &Path) -> Result<Vec<u8>, Error> {
        let mut file = self.open_bounded(path)?;
        let size = file
            .metadata()
            .map_err(|error| inspect_error(path, error))?
            .len();
        let mut context = Context::new(&SHA256);
        let mut bytes = Vec::with_capacity(usize::try_from(size).map_err(|_| {
            Error::Integrity(format!(
                "model artifact {} is too large for this platform",
                path.display()
            ))
        })?);
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let read = read_artifact(&mut file, path, &mut buffer)?;
            if read == 0 {
                break;
            }
            let chunk = buffer.get(..read).ok_or_else(|| {
                Error::Integrity("artifact read exceeded its fixed buffer".to_owned())
            })?;
            context.update(chunk);
            bytes.extend_from_slice(chunk);
            if bytes.len() > MAX_WEIGHT_BYTES {
                return Err(too_large(path));
            }
        }
        self.check_digest(path, context)?;
        Ok(bytes)
    }

    fn open_bounded(&self, path: &Path) -> Result<std::fs::File, Error> {
        let file = std::fs::File::open(path).map_err(|error| {
            Error::Io(format!(
                "cannot read model artifact {}: {error}",
                path.display()
            ))
        })?;
        let size = file
            .metadata()
            .map_err(|error| inspect_error(path, error))?
            .len();
        if size > MAX_WEIGHT_BYTES as u64 {
            return Err(too_large(path));
        }
        Ok(file)
    }

    fn check_digest(&self, path: &Path, context: Context) -> Result<(), Error> {
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

fn read_artifact(file: &mut std::fs::File, path: &Path, buffer: &mut [u8]) -> Result<usize, Error> {
    file.read(buffer).map_err(|error| {
        Error::Io(format!(
            "cannot read model artifact {}: {error}",
            path.display()
        ))
    })
}

fn inspect_error(path: &Path, error: std::io::Error) -> Error {
    Error::Io(format!(
        "cannot inspect model artifact {}: {error}",
        path.display()
    ))
}

fn too_large(path: &Path) -> Error {
    Error::Integrity(format!(
        "model artifact {} exceeds the {MAX_WEIGHT_BYTES}-byte limit",
        path.display()
    ))
}

/// A preflight-owned transport for the one-time artifact fetch.
#[allow(async_fn_in_trait)]
pub trait WeightFetcher: Send + Sync {
    /// Fetch `https://{permit.host()}{path}` and return at most `max_bytes`.
    ///
    /// Implementations must construct the destination from these components,
    /// disable redirects, and stop reading once the bound is reached. The
    /// caller checks the returned length again before writing it to disk.
    async fn fetch(&self, permit: &Permit, path: &str, max_bytes: usize) -> Result<Vec<u8>, Error>;
}

/// The configured sentence model implementation.
///
/// Model selection stays configuration. This trait keeps the provider free
/// of an embedded HTTP client and makes runtime inference an in-process call.
#[allow(async_fn_in_trait)]
pub trait LocalEngine: Send + Sync {
    /// Run CPU inference over the already loaded artifact.
    async fn infer(&self, weights: &[u8], batch: &[&str]) -> Result<Vec<Vector>, Error>;
}

/// The local default. Calling [`EmbeddingProvider::embed`] performs no file
/// download and no network operation.
#[derive(Clone)]
pub struct LocalProvider<E> {
    id: ModelId,
    dims: u16,
    normalized: bool,
    weights: Arc<[u8]>,
    engine: E,
}

impl<E> fmt::Debug for LocalProvider<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LocalProvider")
            .field("id", &self.id)
            .field("dims", &self.dims)
            .field("normalized", &self.normalized)
            .field("weights_bytes", &self.weights.len())
            .field("engine", &type_name::<E>())
            .finish()
    }
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
        mut weights: WeightArtifact,
        models_dir: &Path,
        engine: E,
    ) -> Result<Self, Error> {
        if dims == 0 {
            return Err(Error::Config(
                "an embedding model must declare at least one dimension".to_owned(),
            ));
        }
        weights.validate(models_dir)?;
        weights.path = confined_destination(&weights.path, models_dir, false)?;
        let verified_weights = weights.read_verified_path(&weights.path)?;
        Ok(Self {
            id,
            dims,
            normalized,
            weights: Arc::from(verified_weights),
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
        egress: &Governed<Egress>,
        actor: &Sub,
        fetcher: &impl WeightFetcher,
    ) -> Result<(), Error> {
        artifact.validate(models_dir)?;
        let destination = confined_destination(&artifact.path, models_dir, true)?;
        if artifact.verify_path(&destination).is_ok() {
            return Ok(());
        }
        let endpoint = parse_https_endpoint(&artifact.url)?;
        let permit = egress.permit(actor, &endpoint.host).await?;
        let bytes = fetcher
            .fetch(&permit, &endpoint.path, MAX_WEIGHT_BYTES)
            .await?;
        if bytes.len() > MAX_WEIGHT_BYTES {
            return Err(Error::Integrity(format!(
                "downloaded model artifact exceeds the {MAX_WEIGHT_BYTES}-byte limit"
            )));
        }
        let actual = hex(digest(&SHA256, &bytes).as_ref());
        if actual != artifact.sha256 {
            return Err(Error::Integrity(format!(
                "downloaded model artifact has SHA-256 {actual}, expected {}",
                artifact.sha256
            )));
        }
        let destination = confined_destination(&artifact.path, models_dir, true)?;
        let (directory, destination_name) = open_destination_directory(&destination, models_dir)?;
        let (temporary_name, mut file) = temporary_file(&directory, &destination_name)?;
        if let Err(error) = file.write_all(&bytes) {
            let _ = rustix::fs::unlinkat(&directory, &temporary_name, rustix::fs::AtFlags::empty());
            return Err(Error::Io(format!(
                "cannot write temporary model artifact: {error}"
            )));
        }
        if let Err(error) = file.sync_all() {
            let _ = rustix::fs::unlinkat(&directory, &temporary_name, rustix::fs::AtFlags::empty());
            return Err(Error::Io(format!(
                "cannot sync temporary model artifact: {error}"
            )));
        }
        drop(file);
        rustix::fs::renameat(&directory, &temporary_name, &directory, &destination_name).map_err(
            |error| {
                let _ =
                    rustix::fs::unlinkat(&directory, &temporary_name, rustix::fs::AtFlags::empty());
                Error::Io(format!(
                    "cannot install model artifact {}: {error}",
                    destination.display()
                ))
            },
        )?;
        directory.sync_all().map_err(|error| {
            Error::Io(format!(
                "cannot sync model directory for {}: {error}",
                destination.display()
            ))
        })?;
        artifact.verify_path(&destination)
    }
}

fn confined_destination(
    path: &Path,
    models_dir: &Path,
    create_missing: bool,
) -> Result<PathBuf, Error> {
    match std::fs::symlink_metadata(models_dir) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(Error::Config(format!(
                "configured models directory {} must not be a symlink",
                models_dir.display()
            )));
        }
        Ok(metadata) if !metadata.is_dir() => {
            return Err(Error::Config(format!(
                "configured models directory {} is not a directory",
                models_dir.display()
            )));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && create_missing => {
            std::fs::create_dir_all(models_dir).map_err(|error| {
                Error::Io(format!(
                    "cannot create models directory {}: {error}",
                    models_dir.display()
                ))
            })?;
        }
        Err(error) => {
            return Err(Error::Io(format!(
                "cannot inspect models directory {}: {error}",
                models_dir.display()
            )));
        }
    }
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
                if create_missing {
                    std::fs::create_dir(&current).map_err(|error| {
                        Error::Io(format!(
                            "cannot create model directory {}: {error}",
                            current.display()
                        ))
                    })?;
                } else {
                    return Err(Error::Io(format!(
                        "cannot inspect model directory {}: {error}",
                        current.display()
                    )));
                }
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

fn open_destination_directory(
    destination: &Path,
    models_dir: &Path,
) -> Result<(std::fs::File, OsString), Error> {
    let parent = destination.parent().ok_or_else(|| {
        Error::Config("canonical model artifact has no parent directory".to_owned())
    })?;
    let relative_parent = parent
        .strip_prefix(std::fs::canonicalize(models_dir).map_err(|error| {
            Error::Io(format!(
                "cannot resolve models directory {}: {error}",
                models_dir.display()
            ))
        })?)
        .map_err(|_| {
            Error::Config(format!(
                "model directory {} is outside configured models directory {}",
                parent.display(),
                models_dir.display()
            ))
        })?;
    let root = rustix::fs::openat(
        rustix::fs::CWD,
        models_dir,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::DIRECTORY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .map_err(|error| {
        Error::Io(format!(
            "cannot open models directory {}: {error}",
            models_dir.display()
        ))
    })?;
    let mut directory = std::fs::File::from(root);
    for component in relative_parent.components() {
        let Component::Normal(component) = component else {
            return Err(Error::Config(format!(
                "model directory {} has an invalid component",
                parent.display()
            )));
        };
        let next = rustix::fs::openat(
            &directory,
            component,
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::DIRECTORY
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .map_err(|error| {
            Error::Io(format!(
                "cannot safely open model directory {}: {error}",
                parent.display()
            ))
        })?;
        directory = std::fs::File::from(next);
    }
    let name = destination
        .file_name()
        .ok_or_else(|| Error::Config("model artifact path has no file name".to_owned()))?
        .to_os_string();
    Ok((directory, name))
}

fn temporary_file(
    directory: &std::fs::File,
    destination_name: &OsStr,
) -> Result<(OsString, std::fs::File), Error> {
    for _ in 0..16 {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let candidate = OsString::from(format!(
            "{}.partial-{}-{sequence}",
            destination_name.to_string_lossy(),
            std::process::id()
        ));
        match rustix::fs::openat(
            directory,
            &candidate,
            rustix::fs::OFlags::WRONLY
                | rustix::fs::OFlags::CREATE
                | rustix::fs::OFlags::EXCL
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
        ) {
            Ok(file) => return Ok((candidate, std::fs::File::from(file))),
            Err(rustix::io::Errno::EXIST) => {}
            Err(error) => {
                return Err(Error::Io(format!(
                    "cannot create temporary model artifact: {error}"
                )));
            }
        }
    }
    Err(Error::Io(
        "cannot reserve a temporary model artifact".to_owned(),
    ))
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
        let vectors = self.engine.infer(&self.weights, batch).await?;
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
    fn validate_accepts_artifact_beneath_relative_models_directory() {
        for models in [Path::new("./models"), Path::new("../data/models")] {
            let artifact = WeightArtifact {
                path: models.join("model.bin"),
                url: "https://models.example/model.bin".to_owned(),
                sha256: "0".repeat(64),
            };

            artifact
                .validate(models)
                .expect("relative models directory");
        }
    }

    #[test]
    fn confined_destination_rejects_a_symlinked_subdirectory() {
        let root = tempfile::tempdir().expect("temporary root");
        let models = root.path().join("models");
        let outside = root.path().join("outside");
        std::fs::create_dir_all(&models).expect("models directory");
        std::fs::create_dir_all(&outside).expect("outside directory");
        symlink(&outside, models.join("linked")).expect("symlink fixture");

        let destination = models.join("linked/model.bin");
        assert!(confined_destination(&destination, &models, true).is_err());
        assert!(!outside.join("model.bin").exists());
    }

    #[test]
    fn verification_does_not_create_a_missing_models_directory() {
        let root = tempfile::tempdir().expect("temporary root");
        let models = root.path().join("missing/models");
        let destination = models.join("model.bin");

        assert!(confined_destination(&destination, &models, false).is_err());
        assert!(!models.exists());
    }
}
