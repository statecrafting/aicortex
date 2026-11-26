//! `POST /search` (020 B-4, 018 B-1 to B-9, 025 B-14).
//!
//! The route is a translation: it resolves the scope against the caller first
//! (B-2), so a scope the caller cannot reach is a 404 decided before any store
//! read, builds a recall query, runs it, and returns the results inside 019's
//! envelopes. This crate holds no SQL and no ranking: `aicortex-recall` ranks,
//! and its delivery stage (025) packs the evidence under a token budget that
//! counts the framing. A request that carries none of `unit`, `window_radius`,
//! and `token_budget` does not run that stage (025 B-3) and gets each ranked
//! memory whole.
//!
//! A request that sets `pointers` also receives the entities its query names
//! (026): resolved by alias match in the searched scope, framed by
//! [`crate::pointers`], and counted inside any delivery budget after the
//! evidence is packed, so a pointer never displaces a result.
//!
//! What a deployment configures is held in [`Search`]: the query embedder
//! behind the [`QueryEmbedder`] seam, the clock, the recall and delivery
//! settings, whether traces are persisted (018 B-6), and a hook the cell
//! counts truncated result sets with (018 B-9).

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use aicortex_embed::{EmbeddingProvider, ModelId, Vector};
use aicortex_recall::delivery::Companion;
use aicortex_recall::{
    Clock, Delivery, DeliveryConfig, DeliveryError, DeliveryRequest, EnvelopeInput, Extra, Filter,
    Query, Recall, RecallConfig, RecallResult, SystemClock, TraceStore, frame_memories,
};
use aicortex_store::{MemoryRepo, ScopeId};
use aicortex_types::{Memory, Scope};
use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode;
use rahi_types::{Error, UnixSeconds};

use crate::dto::{FilterIn, ScopeRef, SearchHit, SearchRequest, SearchResponse};
use crate::error::{ApiError, ApiResult};
use crate::pointers::{self, PointerConfig};
use crate::router::{Api, Caller};

/// Results returned when the request names no `k`.
pub const DEFAULT_K: u32 = 10;

/// The most results one search returns.
pub const MAX_K: u32 = 50;

/// The longest time budget a request may name, in milliseconds.
pub const MAX_BUDGET_MS: u64 = 10_000;

/// The longest query, in characters.
pub const MAX_QUERY_CHARS: usize = 2_000;

/// The most values one filter list carries.
pub const MAX_FILTER_VALUES: usize = 32;

/// The model id the placeholder-free "no embedder" provider reports. It names
/// no model a store can have activated, so recall skips the vector channel
/// and says why instead of comparing against another model's vectors.
const UNCONFIGURED_MODEL: &str = "aicortex-search-no-query-embedder";

/// What turns a query into a vector, behind a seam this crate owns.
///
/// The cell supplies one built from its embedding configuration; this crate
/// links no model and opens no connection. The vector channel runs only when
/// the embedder's identity is the active model's (018 B-2): a query only
/// compares against its own model's vectors.
pub trait QueryEmbedder: Send + Sync {
    /// The model identity this embedder produces vectors for.
    fn id(&self) -> ModelId;

    /// The width of its vectors.
    fn dims(&self) -> u16;

    /// Whether its vectors are L2-normalized.
    fn normalized(&self) -> bool;

    /// Embed one query text.
    fn embed<'a>(
        &'a self,
        text: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Vector, Error>> + Send + 'a>>;
}

/// The provider recall is handed: the configured embedder, or one that
/// matches no model.
struct SharedEmbedder {
    embedder: Option<Arc<dyn QueryEmbedder>>,
    unconfigured: ModelId,
}

impl EmbeddingProvider for SharedEmbedder {
    fn id(&self) -> ModelId {
        self.embedder
            .as_ref()
            .map_or_else(|| self.unconfigured.clone(), |inner| inner.id())
    }

    fn dims(&self) -> u16 {
        self.embedder.as_ref().map_or(0, |inner| inner.dims())
    }

    fn normalized(&self) -> bool {
        self.embedder
            .as_ref()
            .is_some_and(|inner| inner.normalized())
    }

    async fn embed(&self, batch: &[&str]) -> Result<Vec<Vector>, Error> {
        let Some(inner) = &self.embedder else {
            return Err(Error::Config(
                "no query embedder is configured for this cell".to_owned(),
            ));
        };
        let mut vectors = Vec::with_capacity(batch.len());
        for text in batch {
            vectors.push(inner.embed(text).await?);
        }
        Ok(vectors)
    }
}

struct SharedClock(Arc<dyn Clock>);

impl Clock for SharedClock {
    fn now(&self) -> UnixSeconds {
        self.0.now()
    }
}

/// What `POST /search` is configured with.
///
/// The default has no query embedder (the vector channel is skipped and the
/// trace says so), the wall clock, the shipped recall and delivery settings,
/// persistence off (018 B-6), and no truncation hook.
#[derive(Clone)]
pub struct Search {
    embedder: Option<Arc<dyn QueryEmbedder>>,
    clock: Arc<dyn Clock>,
    recall: RecallConfig,
    delivery: DeliveryConfig,
    pointers: PointerConfig,
    trace_retention_secs: Option<u64>,
    on_truncated: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl Default for Search {
    fn default() -> Self {
        Self {
            embedder: None,
            clock: Arc::new(SystemClock),
            recall: RecallConfig::default(),
            delivery: DeliveryConfig::default(),
            pointers: PointerConfig::default(),
            trace_retention_secs: None,
            on_truncated: None,
        }
    }
}

impl std::fmt::Debug for Search {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Search")
            .field("embedder", &self.embedder.as_ref().map(|e| e.id()))
            .field("delivery", &self.delivery)
            .field("pointers", &self.pointers)
            .field("trace_retention_secs", &self.trace_retention_secs)
            .finish_non_exhaustive()
    }
}

impl Search {
    /// The same, embedding queries with `embedder`.
    #[must_use]
    pub fn with_embedder(mut self, embedder: Arc<dyn QueryEmbedder>) -> Self {
        self.embedder = Some(embedder);
        self
    }

    /// The same, reading time from `clock` (018 B-8).
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// The same, with other recall settings.
    #[must_use]
    pub const fn with_recall(mut self, recall: RecallConfig) -> Self {
        self.recall = recall;
        self
    }

    /// The same, with other delivery settings (025 B-5, B-8).
    #[must_use]
    pub fn with_delivery(mut self, delivery: DeliveryConfig) -> Self {
        self.delivery = delivery;
        self
    }

    /// The same, with other entity pointer settings, or pointers disabled
    /// (026 B-6, B-8).
    #[must_use]
    pub const fn with_pointers(mut self, pointers: PointerConfig) -> Self {
        self.pointers = pointers;
        self
    }

    /// The same, persisting each query's trace for `retention_secs` (018 B-6).
    #[must_use]
    pub const fn with_trace_retention(mut self, retention_secs: u64) -> Self {
        self.trace_retention_secs = Some(retention_secs);
        self
    }

    /// The same, calling `hook` once for every result set that a channel's
    /// budget cut short, which is what the cell counts (018 B-9).
    #[must_use]
    pub fn with_truncation_hook(mut self, hook: Arc<dyn Fn() + Send + Sync>) -> Self {
        self.on_truncated = Some(hook);
        self
    }
}

/// A delivery refusal as the problem document that names it (025 B-7, B-1).
fn delivery_error(error: DeliveryError) -> ApiError {
    match error {
        DeliveryError::UnknownUnit { .. } | DeliveryError::WindowRadius { .. } => {
            ApiError::invalid(error.to_string())
        }
        DeliveryError::BudgetBelowMinimum { .. } => ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "token_budget_below_minimum",
            error.to_string(),
        ),
        DeliveryError::Store(error) => ApiError::from(error),
        DeliveryError::Config { .. } | DeliveryError::Frame(_) => ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            "this cell could not deliver the evidence",
        ),
    }
}

fn bounded(name: &str, values: &[String]) -> ApiResult<()> {
    if values.len() > MAX_FILTER_VALUES {
        return Err(ApiError::invalid(format!(
            "filter.{name} carries at most {MAX_FILTER_VALUES} values, not {}",
            values.len()
        )));
    }
    Ok(())
}

/// The recall filter a request names, validated against its vocabulary.
fn filter(request: Option<&FilterIn>) -> ApiResult<Filter> {
    let Some(request) = request else {
        return Ok(Filter::default());
    };
    for (name, values) in [
        ("kinds", &request.kinds),
        ("trusts", &request.trusts),
        ("actor_kinds", &request.actor_kinds),
        ("sources", &request.sources),
        ("entities", &request.entities),
        ("include", &request.include),
    ] {
        bounded(name, values)?;
    }
    let extra_statuses = request
        .include
        .iter()
        .map(|status| match status.as_str() {
            "superseded" => Ok(Extra::Superseded),
            "expired" => Ok(Extra::Expired),
            other => Err(ApiError::invalid(format!(
                "filter.include {other:?} is not one of superseded, expired; \
                 quarantined and erased memories cannot be requested"
            ))),
        })
        .collect::<ApiResult<Vec<_>>>()?;
    let filter = Filter {
        kinds: request.kinds.clone(),
        trusts: request.trusts.clone(),
        actor_kinds: request.actor_kinds.clone(),
        sources: request.sources.clone(),
        entities: request.entities.clone(),
        created_from: request.created_from.map(UnixSeconds::new),
        created_to: request.created_to.map(UnixSeconds::new),
        extra_statuses,
    };
    filter.validate()?;
    Ok(filter)
}

/// The ranked memories whole, in envelopes, for a request that asked for no
/// delivery (025 B-3). A memory that is gone or no longer admitted is left
/// out; its place is still in `hits`.
async fn whole(
    api: &Api,
    scope: &Scope,
    result: &RecallResult,
) -> ApiResult<aicortex_recall::FramedMemories> {
    let repo = MemoryRepo::new();
    let admitted = result.trace.filters.status_labels();
    let mut memories: Vec<Memory> = Vec::with_capacity(result.memories.len());
    for recalled in &result.memories {
        if let Some(memory) = repo.get(api.app.store(), scope, recalled.memory_id).await?
            && admitted.contains(&memory.status.label())
        {
            memories.push(memory);
        }
    }
    let ids: Vec<_> = memories.iter().map(|memory| memory.id).collect();
    let erased = repo.origin_erased(api.app.store(), scope, &ids).await?;
    frame_memories(
        memories
            .iter()
            .map(|memory| EnvelopeInput::new(memory, erased.contains(&memory.id))),
    )
    .map_err(|_| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            "a memory could not be enveloped",
        )
    })
}

/// `POST /search`: rank, optionally deliver under a budget, and frame.
pub async fn search(
    State(api): State<Api>,
    caller: Caller,
    body: Bytes,
) -> ApiResult<Json<SearchResponse>> {
    let request: SearchRequest = serde_json::from_slice(&body)
        .map_err(|error| ApiError::invalid(format!("the search request is malformed: {error}")))?;
    let scope = request
        .scope
        .clone()
        .unwrap_or_else(ScopeRef::personal)
        .resolve(caller.sub())?;
    if request.query.trim().is_empty() {
        return Err(ApiError::invalid("query must not be empty"));
    }
    if request.query.chars().count() > MAX_QUERY_CHARS {
        return Err(ApiError::invalid(format!(
            "query is at most {MAX_QUERY_CHARS} characters"
        )));
    }
    let k = request.k.unwrap_or(DEFAULT_K);
    if !(1..=MAX_K).contains(&k) {
        return Err(ApiError::invalid(format!("k is between 1 and {MAX_K}")));
    }
    if let Some(ms) = request.budget_ms
        && !(1..=MAX_BUDGET_MS).contains(&ms)
    {
        return Err(ApiError::invalid(format!(
            "budget_ms is between 1 and {MAX_BUDGET_MS}"
        )));
    }
    let delivery = DeliveryRequest::parse(
        request.unit.as_deref(),
        request.window_radius,
        request.token_budget,
    )
    .map_err(delivery_error)?;
    let config = &api.search;

    let mut query = Query::new(
        scope.clone(),
        request.query.clone(),
        usize::try_from(k).unwrap_or(usize::MAX),
    );
    query.filter = filter(request.filter.as_ref())?;
    if let Some(ms) = request.budget_ms {
        query.budget = std::time::Duration::from_millis(ms);
    }
    // A persisted trace is the full one: it is what is debugged (018 D-2).
    query.explain = request.explain || config.trace_retention_secs.is_some();

    let store = api.app.store();
    let recall = Recall::new(
        store.clone(),
        SharedEmbedder {
            embedder: config.embedder.clone(),
            unconfigured: ModelId::new(UNCONFIGURED_MODEL)?,
        },
        SharedClock(Arc::clone(&config.clock)),
        config.recall,
    );
    let result = recall.recall(&query).await?;
    if result.truncated()
        && let Some(hook) = &config.on_truncated
    {
        hook();
    }

    // Entity pointers (026): only when asked, only in the scope this search
    // was admitted for, and nothing is stored, traced, or logged for them.
    let pointers_disabled = request.pointers && !config.pointers.enabled();
    let block = if request.pointers && config.pointers.enabled() {
        pointers::resolve(
            store,
            &scope,
            &request.query,
            &config.pointers,
            &config.delivery,
        )
        .await?
    } else {
        None
    };

    // The evidence is packed first, as it would be without pointers; the
    // block is fitted into what it leaves (026 B-9).
    let delivered = Delivery::new(store.clone(), config.delivery.clone())
        .deliver_with(
            &scope,
            &result,
            &delivery,
            block.as_ref().map(|block| block as &dyn Companion),
        )
        .await
        .map_err(delivery_error)?;
    let (framed, report, fitted) = match delivered {
        Some(delivered) => (
            delivered.payload,
            Some(delivered.report),
            delivered.companion,
        ),
        None => (whole(&api, &scope, &result).await?, None, None),
    };
    let block = match (block, fitted) {
        (Some(mut block), Some(fitted)) => fitted.keep.map(|keep| {
            block.cut(keep);
            block
        }),
        (block, None) => block,
        (None, Some(_)) => None,
    };
    let trace_id = match config.trace_retention_secs {
        Some(retention) => Some(
            TraceStore::record(
                store,
                &ScopeId::of(&scope),
                &result.trace,
                report.as_ref(),
                config.clock.now(),
                retention,
            )
            .await?,
        ),
        None => None,
    };
    let hits = result
        .memories
        .iter()
        .map(|recalled| SearchHit {
            id: recalled.memory_id,
            rank: recalled.rank,
            score: recalled.score,
            chunk: recalled.chunk,
        })
        .collect();
    Ok(Json(SearchResponse {
        framed,
        hits,
        trace: result.trace,
        trace_id,
        delivery: report,
        pointers: block,
        pointers_disabled,
    }))
}
