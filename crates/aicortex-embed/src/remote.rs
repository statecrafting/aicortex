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
    /// Submit a batch to `https://{permit.host()}[:port]{path}`.
    ///
    /// Implementations must construct the destination from these components,
    /// disable redirects, and never accept a second authority from a response
    /// or model payload.
    async fn embed(
        &self,
        permit: &Permit,
        port: Option<u16>,
        path: &str,
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
    port: Option<u16>,
    path: String,
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
        let parsed = parse_https_endpoint(&endpoint)?;
        let permit = egress.permit(actor, &parsed.host).await?;
        Ok(Self {
            id,
            dims,
            normalized,
            port: parsed.port,
            path: parsed.path,
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
            .embed(&self.permit, self.port, &self.path, &self.id, batch)
            .await?;
        validate_batch(self, batch.len(), &vectors)?;
        Ok(vectors)
    }
}

struct ParsedEndpoint {
    host: String,
    port: Option<u16>,
    path: String,
}

fn parse_https_endpoint(endpoint: &str) -> Result<ParsedEndpoint, Error> {
    let authority = endpoint.strip_prefix("https://").ok_or_else(|| {
        Error::Config(format!(
            "remote embedding endpoint {endpoint:?} must use https"
        ))
    })?;
    let (host_port, path) = authority.find('/').map_or((authority, "/"), |index| {
        (&authority[..index], &authority[index..])
    });
    if host_port.is_empty() || host_port.contains('@') || host_port.starts_with('[') {
        return Err(Error::Config(format!(
            "remote embedding endpoint {endpoint:?} does not name a supported host"
        )));
    }
    let (host, port) = host_port.rsplit_once(':').map_or_else(
        || Ok((host_port, None)),
        |(host, port)| {
            let port = port.parse::<u16>().map_err(|_| {
                Error::Config(format!(
                    "remote embedding endpoint {endpoint:?} has an invalid port"
                ))
            })?;
            if port == 0 {
                return Err(Error::Config(format!(
                    "remote embedding endpoint {endpoint:?} has an invalid port"
                )));
            }
            Ok((host, Some(port)))
        },
    )?;
    if host.is_empty()
        || !host.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'.' || byte == b'-'
        })
        || host.starts_with('.')
        || host.ends_with('.')
        || host.split('.').any(|label| label.is_empty())
    {
        return Err(Error::Config(format!(
            "remote embedding endpoint {endpoint:?} has an invalid lowercase host"
        )));
    }
    if path.contains('#')
        || path.starts_with("//")
        || path
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace())
    {
        return Err(Error::Config(format!(
            "remote embedding endpoint {endpoint:?} has an invalid path"
        )));
    }
    Ok(ParsedEndpoint {
        host: host.to_owned(),
        port,
        path: path.to_owned(),
    })
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_validation_rejects_ambiguous_authorities() {
        for endpoint in [
            "http://models.example/embed",
            "https://models.example:abc/embed",
            "https://a..b/embed",
            "https://models.example./embed",
            "https://models.example//other",
            "https://models.example/embed with-space",
            "https://models.example/embed\r\nX-Injected: 1",
        ] {
            assert!(parse_https_endpoint(endpoint).is_err(), "{endpoint}");
        }
    }

    #[test]
    fn endpoint_is_split_into_admitted_components() {
        let endpoint = parse_https_endpoint("https://models.example:8443/v1/embed?x=1")
            .expect("valid endpoint");
        assert_eq!(endpoint.host, "models.example");
        assert_eq!(endpoint.port, Some(8443));
        assert_eq!(endpoint.path, "/v1/embed?x=1");
    }
}
