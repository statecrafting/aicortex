//! Provider configuration, resolved once at boot (spec 015 B-4, B-5, B-6).
//!
//! The configuration is read from the process environment through rahi's
//! [`EnvReader`], so a test supplies a map. Nothing here opens a socket:
//! [`EmbeddingConfig::check_ceiling`] answers from the manifest alone, which
//! is what lets a deployment check it before it serves and what a chassis
//! preflight extension can call unchanged.
//!
//! | Variable | Meaning |
//! |---|---|
//! | `AICORTEX_EMBED_PROVIDER` | `local` (default), `remote`, or `off` |
//! | `AICORTEX_EMBED_MODEL` | the model identity; absent means no provider |
//! | `AICORTEX_EMBED_DIMS` | vector width, required |
//! | `AICORTEX_EMBED_NORMALIZED` | `true` (default) or `false` |
//! | `AICORTEX_EMBED_MODELS_DIR` | the `models/` directory, default `models` |
//! | `AICORTEX_EMBED_WEIGHTS_FILE`, `_URL`, `_SHA256` | the pinned weights |
//! | `AICORTEX_EMBED_TOKENIZER_FILE`, `_URL`, `_SHA256` | the pinned tokenizer |
//! | `AICORTEX_EMBED_MAX_TOKENS` | token budget per text, default 512 |
//! | `AICORTEX_EMBED_ENDPOINT` | the remote `https` endpoint |

use std::fmt;
use std::path::PathBuf;

use rahi_kernel::{CapabilityKind, Egress, Governed, Manifest, ServiceName};
use rahi_types::{EnvReader, Error, Sub};

use crate::local::{LocalProvider, WeightArtifact};
use crate::provider::{EmbeddingProvider, ModelId, Vector};
use crate::remote::{EmbeddingTransport, RemoteProvider, parse_https_endpoint};
use crate::static_model::{DEFAULT_MAX_TOKENS, StaticEmbeddingEngine};

/// The manifest service that holds the embedding pipeline's egress grants.
pub const EMBEDDING_SERVICE: &str = "embedding";

/// Provider selection.
pub const ENV_PROVIDER: &str = "AICORTEX_EMBED_PROVIDER";
/// Model identity.
pub const ENV_MODEL: &str = "AICORTEX_EMBED_MODEL";
/// Vector width.
pub const ENV_DIMS: &str = "AICORTEX_EMBED_DIMS";
/// Whether the provider returns L2-normalized vectors.
pub const ENV_NORMALIZED: &str = "AICORTEX_EMBED_NORMALIZED";
/// The directory holding model artifacts.
pub const ENV_MODELS_DIR: &str = "AICORTEX_EMBED_MODELS_DIR";
/// Weights file name beneath the models directory.
pub const ENV_WEIGHTS_FILE: &str = "AICORTEX_EMBED_WEIGHTS_FILE";
/// Weights fetch URL.
pub const ENV_WEIGHTS_URL: &str = "AICORTEX_EMBED_WEIGHTS_URL";
/// Weights SHA-256 pin.
pub const ENV_WEIGHTS_SHA256: &str = "AICORTEX_EMBED_WEIGHTS_SHA256";
/// Tokenizer file name beneath the models directory.
pub const ENV_TOKENIZER_FILE: &str = "AICORTEX_EMBED_TOKENIZER_FILE";
/// Tokenizer fetch URL.
pub const ENV_TOKENIZER_URL: &str = "AICORTEX_EMBED_TOKENIZER_URL";
/// Tokenizer SHA-256 pin.
pub const ENV_TOKENIZER_SHA256: &str = "AICORTEX_EMBED_TOKENIZER_SHA256";
/// Token budget per text.
pub const ENV_MAX_TOKENS: &str = "AICORTEX_EMBED_MAX_TOKENS";
/// Remote endpoint.
pub const ENV_ENDPOINT: &str = "AICORTEX_EMBED_ENDPOINT";

/// The identity every vector from one provider carries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelShape {
    /// Model identity.
    pub id: ModelId,
    /// Vector width.
    pub dims: u16,
    /// Whether vectors are L2-normalized.
    pub normalized: bool,
}

/// The local provider's pinned artifacts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalConfig {
    /// What the vectors are.
    pub shape: ModelShape,
    /// The `models/` directory.
    pub models_dir: PathBuf,
    /// The embedding table.
    pub weights: WeightArtifact,
    /// The WordPiece vocabulary.
    pub tokenizer: WeightArtifact,
    /// Token budget per text.
    pub max_tokens: usize,
}

/// The remote provider's endpoint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteConfig {
    /// What the vectors are.
    pub shape: ModelShape,
    /// The `https` endpoint; its host must be in the manifest ceiling.
    pub endpoint: String,
}

/// The configured provider, before it is booted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EmbeddingConfig {
    /// No provider: capture stages no embedding work (spec 015 D-21).
    Disabled,
    /// In-process CPU inference, the default (B-5).
    Local(LocalConfig),
    /// A governed remote endpoint, opt-in (B-6).
    Remote(RemoteConfig),
}

/// A configured provider the manifest ceiling does not admit.
///
/// The name is stable so an operator, a test, and a chassis preflight
/// extension can all refer to the same failure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapabilityFailure {
    /// The named check, for example `embedding.remote.egress`.
    pub check: &'static str,
    /// The manifest service whose grant is missing.
    pub service: String,
    /// The host that was not admitted.
    pub host: String,
}

impl fmt::Display for CapabilityFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}: the manifest ceiling grants service {} no http.egress to {}; add the host to \
             resources.egress and grant it to the service to enable this provider",
            self.check, self.service, self.host
        )
    }
}

impl std::error::Error for CapabilityFailure {}

impl From<CapabilityFailure> for Error {
    fn from(failure: CapabilityFailure) -> Self {
        Self::Denied(failure.to_string())
    }
}

impl EmbeddingConfig {
    /// Read the configuration.
    ///
    /// # Errors
    ///
    /// [`Error::Config`] naming the variable that is missing or malformed.
    pub fn from_env(env: &dyn EnvReader) -> Result<Self, Error> {
        let provider = text(env, ENV_PROVIDER);
        let kind = match provider.as_deref() {
            None | Some("local") => Kind::Local,
            Some("remote") => Kind::Remote,
            Some("off") => return Ok(Self::Disabled),
            Some(other) => {
                return Err(Error::Config(format!(
                    "{ENV_PROVIDER} {other:?} is not local, remote, or off"
                )));
            }
        };
        let Some(model) = text(env, ENV_MODEL) else {
            if provider.is_some() {
                return Err(Error::Config(format!(
                    "{ENV_PROVIDER} selects a provider but {ENV_MODEL} is not set"
                )));
            }
            return Ok(Self::Disabled);
        };
        let shape = ModelShape {
            id: ModelId::new(model)?,
            dims: required(env, ENV_DIMS)?.parse().map_err(|_| {
                Error::Config(format!("{ENV_DIMS} must be an integer in 1..=65535"))
            })?,
            normalized: match text(env, ENV_NORMALIZED).as_deref() {
                None | Some("true") => true,
                Some("false") => false,
                Some(_) => {
                    return Err(Error::Config(format!(
                        "{ENV_NORMALIZED} must be true or false"
                    )));
                }
            },
        };
        if shape.dims == 0 {
            return Err(Error::Config(format!("{ENV_DIMS} must be at least 1")));
        }
        match kind {
            Kind::Remote => Ok(Self::Remote(RemoteConfig {
                shape,
                endpoint: required(env, ENV_ENDPOINT)?,
            })),
            Kind::Local => {
                let models_dir =
                    PathBuf::from(text(env, ENV_MODELS_DIR).unwrap_or_else(|| "models".to_owned()));
                let artifact = |file, url, sha256| -> Result<WeightArtifact, Error> {
                    Ok(WeightArtifact {
                        path: models_dir.join(required(env, file)?),
                        url: required(env, url)?,
                        sha256: required(env, sha256)?,
                    })
                };
                let weights = artifact(ENV_WEIGHTS_FILE, ENV_WEIGHTS_URL, ENV_WEIGHTS_SHA256)?;
                let tokenizer =
                    artifact(ENV_TOKENIZER_FILE, ENV_TOKENIZER_URL, ENV_TOKENIZER_SHA256)?;
                weights.validate(&models_dir)?;
                tokenizer.validate(&models_dir)?;
                let max_tokens = match text(env, ENV_MAX_TOKENS) {
                    None => DEFAULT_MAX_TOKENS,
                    Some(raw) => raw.parse().map_err(|_| {
                        Error::Config(format!("{ENV_MAX_TOKENS} must be a whole number"))
                    })?,
                };
                Ok(Self::Local(LocalConfig {
                    shape,
                    models_dir,
                    weights,
                    tokenizer,
                    max_tokens,
                }))
            }
        }
    }

    /// Whether the manifest ceiling admits this configuration (B-5, B-6).
    ///
    /// A remote provider needs `http.egress` to its endpoint's host. A local
    /// provider needs none while its artifacts are present; an absent
    /// artifact needs its fetch host. The question is answered from the
    /// manifest and the artifact's existence, so no socket is opened and no
    /// boot is attempted. Whether a present artifact matches its pin is
    /// [`Self::check`]'s question, not the ceiling's.
    ///
    /// # Errors
    ///
    /// [`CapabilityFailure`] naming the check, the service, and the host. An
    /// endpoint that is not a supported `https` host is refused under the
    /// `.endpoint` check name and is never looked up in the manifest, so a
    /// malformed endpoint cannot be admitted by a malformed ceiling entry.
    pub fn check_ceiling(&self, manifest: &Manifest) -> Result<(), CapabilityFailure> {
        match self {
            Self::Disabled => Ok(()),
            Self::Remote(remote) => admit_endpoint(
                manifest,
                "embedding.remote.egress",
                "embedding.remote.endpoint",
                &remote.endpoint,
            ),
            Self::Local(local) => {
                for artifact in [&local.weights, &local.tokenizer] {
                    if is_absent(artifact) {
                        admit_endpoint(
                            manifest,
                            "embedding.weights.egress",
                            "embedding.weights.endpoint",
                            &artifact.url,
                        )?;
                    }
                }
                Ok(())
            }
        }
    }

    /// The full boot-time check: configuration shape, then the ceiling.
    ///
    /// This is the function a chassis preflight extension mounts. It never
    /// opens a socket and never mutates.
    ///
    /// # Errors
    ///
    /// [`Error::Config`] for an endpoint that is not a supported `https`
    /// host; [`Error::Denied`] carrying a [`CapabilityFailure`] when the
    /// ceiling does not admit the provider; [`Error::Integrity`] or
    /// [`Error::Io`] naming a present local artifact that does not match its
    /// pin or cannot be read, which is reported as what it is rather than as
    /// a missing egress grant.
    pub fn check(&self, manifest: &Manifest) -> Result<(), Error> {
        if let Self::Remote(remote) = self {
            parse_https_endpoint(&remote.endpoint)?;
        }
        self.check_ceiling(manifest).map_err(Error::from)?;
        if let Self::Local(local) = self {
            for artifact in [&local.weights, &local.tokenizer] {
                if !is_absent(artifact) {
                    artifact.verify()?;
                }
            }
        }
        Ok(())
    }

    /// The vectors this configuration promises, when a provider is set.
    #[must_use]
    pub const fn shape(&self) -> Option<&ModelShape> {
        match self {
            Self::Disabled => None,
            Self::Local(local) => Some(&local.shape),
            Self::Remote(remote) => Some(&remote.shape),
        }
    }

    /// Resolve the configured provider, once, at boot (B-4).
    ///
    /// A remote provider boots through `egress`, so a host the ceiling does
    /// not admit is refused here and never at first use (B-6).
    ///
    /// # Errors
    ///
    /// Artifact verification, tokenizer, or egress errors; [`Error::Config`]
    /// when a remote provider is configured without governed egress.
    pub async fn boot<T: EmbeddingTransport>(
        &self,
        egress: Option<&Governed<Egress>>,
        actor: &Sub,
        transport: T,
    ) -> Result<Option<ConfiguredProvider<T>>, Error> {
        match self {
            Self::Disabled => Ok(None),
            Self::Local(local) => {
                let engine = StaticEmbeddingEngine::from_artifact(
                    &local.tokenizer,
                    &local.models_dir,
                    local.shape.normalized,
                    local.max_tokens,
                )?;
                let provider = LocalProvider::boot(
                    local.shape.id.clone(),
                    local.shape.dims,
                    local.shape.normalized,
                    local.weights.clone(),
                    &local.models_dir,
                    engine,
                )?;
                Ok(Some(ConfiguredProvider::Local(provider)))
            }
            Self::Remote(remote) => {
                let egress = egress.ok_or_else(|| {
                    Error::Config(
                        "a remote embedding provider needs the kernel's governed egress".to_owned(),
                    )
                })?;
                let provider = RemoteProvider::boot(
                    remote.shape.id.clone(),
                    remote.shape.dims,
                    remote.shape.normalized,
                    remote.endpoint.clone(),
                    egress,
                    actor,
                    transport,
                )
                .await?;
                Ok(Some(ConfiguredProvider::Remote(provider)))
            }
        }
    }
}

/// The provider chosen by configuration.
#[derive(Debug)]
pub enum ConfiguredProvider<T> {
    /// In-process inference.
    Local(LocalProvider<StaticEmbeddingEngine>),
    /// Governed remote inference.
    Remote(RemoteProvider<T>),
}

impl<T: EmbeddingTransport> EmbeddingProvider for ConfiguredProvider<T> {
    fn id(&self) -> ModelId {
        match self {
            Self::Local(provider) => provider.id(),
            Self::Remote(provider) => provider.id(),
        }
    }

    fn dims(&self) -> u16 {
        match self {
            Self::Local(provider) => provider.dims(),
            Self::Remote(provider) => provider.dims(),
        }
    }

    fn normalized(&self) -> bool {
        match self {
            Self::Local(provider) => provider.normalized(),
            Self::Remote(provider) => provider.normalized(),
        }
    }

    async fn embed(&self, batch: &[&str]) -> Result<Vec<Vector>, Error> {
        match self {
            Self::Local(provider) => provider.embed(batch).await,
            Self::Remote(provider) => provider.embed(batch).await,
        }
    }
}

/// A transport for builds that link no remote client.
///
/// Booting a remote provider with it still performs the governed egress
/// admission, then every call fails with a configuration error.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoTransport;

impl EmbeddingTransport for NoTransport {
    async fn embed(
        &self,
        _permit: &rahi_kernel::Permit,
        _path: &str,
        _model: &ModelId,
        _batch: &[&str],
    ) -> Result<Vec<Vector>, Error> {
        Err(Error::Config(
            "this build links no remote embedding transport".to_owned(),
        ))
    }
}

enum Kind {
    Local,
    Remote,
}

fn text(env: &dyn EnvReader, key: &str) -> Option<String> {
    env.get(key)
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn required(env: &dyn EnvReader, key: &str) -> Result<String, Error> {
    text(env, key).ok_or_else(|| Error::Config(format!("{key} is required and not set")))
}

fn is_absent(artifact: &WeightArtifact) -> bool {
    matches!(
        std::fs::symlink_metadata(&artifact.path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound
    )
}

fn admit_endpoint(
    manifest: &Manifest,
    egress_check: &'static str,
    endpoint_check: &'static str,
    endpoint: &str,
) -> Result<(), CapabilityFailure> {
    match parse_https_endpoint(endpoint) {
        Ok(parsed) => admit(manifest, egress_check, &parsed.host),
        Err(_) => Err(CapabilityFailure {
            check: endpoint_check,
            service: EMBEDDING_SERVICE.to_owned(),
            host: endpoint.to_owned(),
        }),
    }
}

fn admit(manifest: &Manifest, check: &'static str, host: &str) -> Result<(), CapabilityFailure> {
    let granted = ServiceName::parse(EMBEDDING_SERVICE)
        .is_ok_and(|service| manifest.covers(&service, CapabilityKind::HttpEgress, host));
    if granted {
        Ok(())
    } else {
        Err(CapabilityFailure {
            check,
            service: EMBEDDING_SERVICE.to_owned(),
            host: host.to_owned(),
        })
    }
}
