//! The embedding operator surface (spec 015 B-4, B-9).
//!
//! The chassis mounts these routes under `/operator` behind the operator
//! role, so no handler here authenticates anyone. They are glue: each one
//! decodes a request, calls a function of `aicortex-embed`, and returns the
//! chassis's one error mapping.
//!
//! - `GET  /embedding/status?scope_id=` the preflight report for one scope.
//! - `POST /embedding/activate` check the manifest ceiling, resolve the
//!   configured provider, and make its model the active revision.
//! - `POST /embedding/reembed` stage one bounded re-embedding pass.
//! - `POST /embedding/drop` drop an inactive revision once coverage is full.
//!
//! The worker is not started here. `embedding_service` mounts it as a managed
//! service of `aicortex serve` (rahi spec 047, D-25), and `embedding_preflight`
//! contributes `app.embedding` to `aicortex preflight` (rahi spec 049). These
//! routes remain the scope-bound view of one scope (D-18).

#![forbid(unsafe_code)]

use std::sync::Arc;

use aicortex_embed::{
    EMBEDDING_SERVICE, EmbeddingConfig, EmbeddingPreflight, NoTransport, operator, unix_now,
};
use axum::extract::{Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use rahi_edge::{AppState, EdgeError, EdgeResult};
use rahi_kernel::{CapabilityKind, Egress, Governed};
use rahi_types::{EnvReader, Error, Sub};
use serde::Deserialize;
use serde_json::{Value, json};

/// The operator subject recorded against egress admission at activation.
const OPERATOR_ACTOR: &str = "aicortex-operator";

/// The re-embedding batch a request without a `limit` stages.
const DEFAULT_REEMBED_LIMIT: u32 = 100;
const _: () = assert!(DEFAULT_REEMBED_LIMIT <= aicortex_embed::MAX_REEMBED_BATCH);

/// What the operator routes close over.
///
/// The configuration is read once, when the router is built, and a malformed
/// one is held as text so each route can answer with it instead of the
/// process refusing to build a router (a `Cell` cannot fail there).
#[derive(Clone, Debug)]
pub struct Embedding {
    app: AppState,
    config: Arc<Result<EmbeddingConfig, String>>,
}

impl Embedding {
    /// Build the surface state from the chassis state and an environment.
    #[must_use]
    pub fn new(app: AppState, env: &dyn EnvReader) -> Self {
        Self {
            app,
            config: Arc::new(EmbeddingConfig::from_env(env).map_err(|error| error.to_string())),
        }
    }

    fn config(&self) -> Result<&EmbeddingConfig, Error> {
        self.config
            .as_ref()
            .as_ref()
            .map_err(|message| Error::Config(message.clone()))
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScopeQuery {
    scope_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReembedRequest {
    scope_id: String,
    cursor: Option<String>,
    limit: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DropRequest {
    scope_id: String,
    revision: u32,
}

/// Add the embedding routes to `router`.
pub fn mount(router: Router<Embedding>) -> Router<Embedding> {
    router
        .route("/embedding/status", get(status))
        .route("/embedding/activate", post(activate))
        .route("/embedding/reembed", post(reembed))
        .route("/embedding/drop", post(drop_revision))
}

async fn status(
    State(embedding): State<Embedding>,
    Query(query): Query<ScopeQuery>,
) -> EdgeResult<Json<Value>> {
    let report =
        EmbeddingPreflight::read(embedding.app.store(), &query.scope_id, unix_now()).await?;
    Ok(Json(json!({
        "active": report.active.as_ref().map(|model| json!({
            "model_id": model.model_id.as_str(),
            "revision": model.revision,
            "dims": model.dims,
            "normalized": model.normalized,
        })),
        "queue": {
            "pending": report.queue.pending,
            "dead": report.queue.dead,
            "quarantined": report.queue.quarantined,
            "oldest_pending_age_seconds": report.queue.oldest_pending_age_seconds,
        },
        "coverage": report.coverage.iter().map(|item| json!({
            "revision": item.revision,
            "embedded": item.embedded,
            "total": item.total,
        })).collect::<Vec<_>>(),
        "live_memories": report.live_memories,
        "warning": report.readiness_warning(),
    })))
}

async fn activate(State(embedding): State<Embedding>) -> EdgeResult<Json<Value>> {
    let config = embedding.config()?;
    if matches!(config, EmbeddingConfig::Remote(_)) {
        // The ceiling check still runs first so a missing grant is named,
        // then the absent transport is reported rather than activating a
        // model nothing can call.
        config.check(embedding.app.kernel().manifest())?;
        return Err(EdgeError(Error::Config(
            "this build links no remote embedding transport".to_owned(),
        )));
    }
    let egress = Governed::new(
        embedding.app.kernel(),
        EMBEDDING_SERVICE,
        CapabilityKind::HttpEgress,
        "*",
        Egress,
    )
    .ok();
    let activation = operator::activate_configured(
        config,
        embedding.app.kernel().manifest(),
        egress.as_ref(),
        &Sub::new(OPERATOR_ACTOR),
        NoTransport,
        embedding.app.store(),
        unix_now(),
    )
    .await?;
    Ok(Json(json!({
        "model_id": activation.model.model_id.as_str(),
        "revision": activation.model.revision,
        "changed": activation.changed,
    })))
}

async fn reembed(
    State(embedding): State<Embedding>,
    Json(request): Json<ReembedRequest>,
) -> EdgeResult<Json<Value>> {
    let batch = operator::reembed_scope(
        embedding.app.store(),
        &request.scope_id,
        request.cursor.as_deref(),
        request.limit.unwrap_or(DEFAULT_REEMBED_LIMIT),
        unix_now(),
    )
    .await?;
    Ok(Json(json!({
        "staged": batch.staged,
        "cursor": batch.cursor.map(|id| id.to_string()),
        "more": batch.more,
    })))
}

async fn drop_revision(
    State(embedding): State<Embedding>,
    Json(request): Json<DropRequest>,
) -> EdgeResult<Json<Value>> {
    operator::drop_revision(embedding.app.store(), &request.scope_id, request.revision).await?;
    Ok(Json(json!({ "dropped": request.revision })))
}
