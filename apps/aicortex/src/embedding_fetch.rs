//! The one-time model fetch at `serve` boot (spec 015 B-5, D-32).
//!
//! This file is the cell's only governed egress call site (spec 010 B-8). A
//! request leaves the process only after the kernel has admitted its host
//! under the `embedding` service's `http.egress` grant: the client is handed
//! a [`Permit`] and builds the destination from it, so there is no path from
//! configuration to a socket that skips adjudication.
//!
//! The fetch runs once, before the worker binds. An artifact already present
//! and matching its pin is never fetched, and a fetched one is verified
//! against its pin before it is renamed into place
//! ([`LocalProvider::ensure_weights`]). The shipped manifest declares no
//! egress host, so a deployment that has not widened its ceiling makes no
//! outbound call: `serve` refuses to start when an artifact is absent and its
//! host is not granted, and `preflight` warns on the absence.

#![forbid(unsafe_code)]

use std::net::SocketAddr;
use std::time::Duration;

use aicortex_embed::{EmbeddingConfig, LocalProvider, WeightArtifact, WeightFetcher};
use rahi_kernel::{Egress, Governed, Permit};
use rahi_types::{Error, Result, Sub};

/// How long a connection may take to establish.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// How long one artifact download may take end to end.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(600);

/// The actor recorded against the kernel's admission of a fetch host.
const FETCH_ACTOR: &str = "aicortex-embedding-fetch";

/// The HTTPS transport the governed fetch hands its permit to.
///
/// Redirects are refused, so the admitted host is the only host reached;
/// proxies from the environment are ignored for the same reason; and the
/// body is read in chunks and abandoned as soon as it passes the bound.
#[derive(Clone, Debug)]
pub struct HttpsFetcher {
    client: reqwest::Client,
}

impl HttpsFetcher {
    /// A fetcher that trusts the platform's roots.
    ///
    /// # Errors
    ///
    /// [`Error::Config`] when the TLS client cannot be built.
    pub fn new() -> Result<Self> {
        Self::build(reqwest::Client::builder())
    }

    /// A fetcher for a private mirror: `root_pem` is trusted in addition to
    /// the platform's roots, and `host` resolves to `address` instead of
    /// through DNS. The host is still admitted by the kernel and still named
    /// in the request, so the mirror must present a certificate for it.
    ///
    /// # Errors
    ///
    /// [`Error::Config`] for an unreadable certificate or a client that
    /// cannot be built.
    pub fn for_mirror(root_pem: &[u8], host: &str, address: SocketAddr) -> Result<Self> {
        let root = reqwest::Certificate::from_pem(root_pem)
            .map_err(|error| Error::Config(format!("cannot read the mirror's root: {error}")))?;
        Self::build(
            reqwest::Client::builder()
                .tls_certs_merge([root])
                .resolve(host, address),
        )
    }

    fn build(builder: reqwest::ClientBuilder) -> Result<Self> {
        let client = builder
            .https_only(true)
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(DOWNLOAD_TIMEOUT)
            .build()
            .map_err(|error| Error::Config(format!("cannot build the fetch client: {error}")))?;
        Ok(Self { client })
    }
}

impl WeightFetcher for HttpsFetcher {
    async fn fetch(&self, permit: &Permit, path: &str, max_bytes: usize) -> Result<Vec<u8>> {
        let url = format!("https://{}{path}", permit.host());
        let mut response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|error| Error::Io(format!("cannot fetch {url}: {error}")))?;
        let status = response.status();
        if !status.is_success() {
            return Err(Error::Io(format!("{url} answered {status}")));
        }
        let too_large =
            || Error::Integrity(format!("{url} is larger than the {max_bytes}-byte limit"));
        if response
            .content_length()
            .is_some_and(|length| length > max_bytes as u64)
        {
            return Err(too_large());
        }
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|error| Error::Io(format!("cannot read {url}: {error}")))?
        {
            if body.len().saturating_add(chunk.len()) > max_bytes {
                return Err(too_large());
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }
}

/// The governed fetch: the `embedding` service's egress facade and the
/// transport it admits hosts for.
#[derive(Clone)]
pub struct WeightFetch {
    egress: Governed<Egress>,
    fetcher: HttpsFetcher,
}

impl std::fmt::Debug for WeightFetch {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WeightFetch")
            .field("service", &self.egress.service())
            .field("fetcher", &self.fetcher)
            .finish()
    }
}

impl WeightFetch {
    /// Pair a facade with a transport.
    #[must_use]
    pub const fn new(egress: Governed<Egress>, fetcher: HttpsFetcher) -> Self {
        Self { egress, fetcher }
    }

    /// Install every pinned artifact of a local configuration that is not
    /// already present and valid, and return how many were fetched.
    ///
    /// Any other configuration needs nothing and fetches nothing.
    ///
    /// # Errors
    ///
    /// [`Error::Denied`] when the kernel does not admit an artifact's host;
    /// [`Error::Io`] for a failed download or install; [`Error::Integrity`]
    /// for a body over the bound or one that does not match its pin.
    pub async fn provision(&self, config: &EmbeddingConfig) -> Result<usize> {
        let EmbeddingConfig::Local(local) = config else {
            return Ok(0);
        };
        let actor = Sub::new(FETCH_ACTOR);
        let mut fetched = 0;
        for artifact in [&local.weights, &local.tokenizer] {
            if needs_fetch(artifact) {
                LocalProvider::<()>::ensure_weights(
                    artifact,
                    &local.models_dir,
                    &self.egress,
                    &actor,
                    &self.fetcher,
                )
                .await?;
                fetched += 1;
            }
        }
        Ok(fetched)
    }
}

/// Whether a configuration has an artifact that is not present and valid.
#[must_use]
pub fn needs_provisioning(config: &EmbeddingConfig) -> bool {
    match config {
        EmbeddingConfig::Local(local) => [&local.weights, &local.tokenizer]
            .into_iter()
            .any(needs_fetch),
        EmbeddingConfig::Disabled | EmbeddingConfig::Remote(_) => false,
    }
}

fn needs_fetch(artifact: &WeightArtifact) -> bool {
    artifact.verify().is_err()
}
