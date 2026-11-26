//! What the cell hands `POST /search` (spec 018 B-2, B-9, 020 D-19).
//!
//! The API crate links no model and no metrics registry, so the two things a
//! search needs from the process are supplied here: a query embedder built
//! from the same embedding configuration the worker reads, and a counter of
//! truncated result sets on the chassis's metrics registry.
//!
//! A deployment with no provider, or with a remote one (this build links no
//! remote transport, 015 D-27), searches without the vector channel and the
//! trace says why. The local provider boots on the first query, not at
//! startup, and a failed boot is retried by the next query, so a model whose
//! artifacts are still being fetched does not wedge search.

#![forbid(unsafe_code)]

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use aicortex_api::{QueryEmbedder, Search};
use aicortex_embed::{
    ConfiguredProvider, EmbeddingConfig, EmbeddingProvider, ModelId, NoTransport, Vector,
};
use prometheus::{IntCounter, Registry};
use rahi_types::{EnvReader, Error, Sub};

/// The counter of result sets a channel's budget cut short (018 B-9).
pub const TRUNCATED_TOTAL: &str = "aicortex_recall_truncated_total";

/// The subject recorded against provider boot.
const ACTOR: &str = "aicortex-search";

type Booted = Arc<ConfiguredProvider<NoTransport>>;

/// The local provider, booted on first use.
struct LocalQueryEmbedder {
    config: EmbeddingConfig,
    id: ModelId,
    dims: u16,
    normalized: bool,
    booted: Mutex<Option<Booted>>,
}

impl LocalQueryEmbedder {
    async fn provider(&self) -> Result<Booted, Error> {
        if let Some(provider) = self
            .booted
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
        {
            return Ok(provider);
        }
        // Booted outside the lock: two first queries may both boot, and the
        // second one's provider is dropped.
        let provider = self
            .config
            .boot(None, &Sub::new(ACTOR), NoTransport)
            .await?
            .ok_or_else(|| Error::Config("no embedding provider is configured".to_owned()))?;
        let mut slot = self.booted.lock().unwrap_or_else(PoisonError::into_inner);
        Ok(Arc::clone(slot.get_or_insert_with(|| Arc::new(provider))))
    }
}

impl QueryEmbedder for LocalQueryEmbedder {
    fn id(&self) -> ModelId {
        self.id.clone()
    }

    fn dims(&self) -> u16 {
        self.dims
    }

    fn normalized(&self) -> bool {
        self.normalized
    }

    fn embed<'a>(
        &'a self,
        text: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Vector, Error>> + Send + 'a>> {
        Box::pin(async move {
            let provider = self.provider().await?;
            provider
                .embed(&[text])
                .await?
                .pop()
                .ok_or_else(|| Error::Integrity("the provider returned no vector".to_owned()))
        })
    }
}

/// The query embedder `config` calls for, when this build can serve one.
fn embedder(config: EmbeddingConfig) -> Option<Arc<dyn QueryEmbedder>> {
    let shape = match &config {
        EmbeddingConfig::Local(_) => config.shape()?.clone(),
        EmbeddingConfig::Disabled | EmbeddingConfig::Remote(_) => return None,
    };
    Some(Arc::new(LocalQueryEmbedder {
        config,
        id: shape.id,
        dims: shape.dims,
        normalized: shape.normalized,
        booted: Mutex::new(None),
    }))
}

/// The truncation counter, created and registered once per process.
fn truncated(registry: Option<&Registry>) -> IntCounter {
    static COUNTER: OnceLock<IntCounter> = OnceLock::new();
    COUNTER
        .get_or_init(|| {
            let counter = IntCounter::new(
                TRUNCATED_TOTAL,
                "Search result sets a channel's time budget cut short",
            )
            .unwrap_or_else(|_| unreachable!("the metric name and help are valid"));
            if let Some(registry) = registry {
                // A name already on the registry keeps the counter working
                // unexported rather than failing a route build.
                let _ = registry.register(Box::new(counter.clone()));
            }
            counter
        })
        .clone()
}

/// The `POST /search` configuration for this process.
///
/// A malformed embedding configuration is not repeated here: `serve` refuses
/// to start on it when it composes the worker, so search just has no embedder.
#[must_use]
pub fn search(env: &dyn EnvReader) -> Search {
    let obs = rahi_edge::obs::current();
    let counter = truncated(obs.as_ref().map(|obs| obs.metrics().registry()));
    let search = Search::default().with_truncation_hook(Arc::new(move || counter.inc()));
    match EmbeddingConfig::from_env(env).ok().and_then(embedder) {
        Some(embedder) => search.with_embedder(embedder),
        None => search,
    }
}
