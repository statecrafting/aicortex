//! Remote inference through the kernel's egress decision.

use rahi_kernel::{Egress, Governed, Permit};
use rahi_types::{Error, Sub};

use crate::provider::{EmbeddingProvider, ModelId, Vector, validate_batch};

/// The application-selected HTTP client.
///
/// The transport receives the kernel permit and cannot be called by
/// [`RemoteProvider`] until boot has admitted the configured host.
#[allow(async_fn_in_trait)]
pub trait EmbeddingTransport: Send + Sync {
    /// Submit a batch using `permit` as evidence for the destination.
    async fn embed(
        &self,
        permit: &Permit,
        endpoint: &str,
        model: &ModelId,
        batch: &[&str],
    ) -> Result<Vec<Vector>, Error>;
}

/// An opt-in provider whose destination is admitted once at boot.
#[derive(Clone, Debug)]
pub struct RemoteProvider<T> {
    id: ModelId,
    dims: u16,
    normalized: bool,
    endpoint: String,
    permit: Permit,
    transport: T,
}

impl<T> RemoteProvider<T> {
    /// Resolve the configured endpoint through `Governed<Egress>`.
    ///
    /// This is the boot boundary. A host absent from the manifest ceiling is
    /// denied here, before the provider can process a memory.
    ///
    /// # Errors
    ///
    /// [`Error::Config`] for an invalid HTTPS endpoint or zero dimensions;
    /// [`Error::Denied`] when the manifest does not admit its host.
    pub async fn boot(
        id: ModelId,
        dims: u16,
        normalized: bool,
        endpoint: impl Into<String>,
        egress: &Governed<Egress>,
        actor: &Sub,
        transport: T,
    ) -> Result<Self, Error> {
        if dims == 0 {
            return Err(Error::Config(
                "an embedding model must declare at least one dimension".to_owned(),
            ));
        }
        let endpoint = endpoint.into();
        let host = https_host(&endpoint)?;
        let permit = egress.permit(actor, host).await?;
        Ok(Self {
            id,
            dims,
            normalized,
            endpoint,
            permit,
            transport,
        })
    }

    /// The admitted host, for preflight reporting.
    #[must_use]
    pub fn host(&self) -> &str {
        self.permit.host()
    }
}

impl<T: EmbeddingTransport> EmbeddingProvider for RemoteProvider<T> {
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
        let vectors = self
            .transport
            .embed(&self.permit, &self.endpoint, &self.id, batch)
            .await?;
        validate_batch(self, batch.len(), &vectors)?;
        Ok(vectors)
    }
}

fn https_host(endpoint: &str) -> Result<&str, Error> {
    let authority = endpoint.strip_prefix("https://").ok_or_else(|| {
        Error::Config(format!(
            "remote embedding endpoint {endpoint:?} must use https"
        ))
    })?;
    let host_port = authority.split('/').next().unwrap_or_default();
    if host_port.is_empty() || host_port.contains('@') || host_port.starts_with('[') {
        return Err(Error::Config(format!(
            "remote embedding endpoint {endpoint:?} does not name a supported host"
        )));
    }
    let host = host_port.split(':').next().unwrap_or_default();
    if host.is_empty()
        || !host.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'.' || byte == b'-'
        })
    {
        return Err(Error::Config(format!(
            "remote embedding endpoint {endpoint:?} has an invalid lowercase host"
        )));
    }
    Ok(host)
}
