//! The router the cell returns from `routes`, and what its handlers share
//! (B-1, B-3, B-9).
//!
//! Nothing here constructs a server, a listener, or a middleware chain: the
//! chassis's edge wraps this router in its observation, security headers,
//! CSRF check, rate limiter, and stream gate, and its bearer layer resolves
//! the credential before any handler runs. This file adds only the scope gate
//! per route, through the chassis's own `RequireScope`.

use std::sync::Arc;

use aicortex_gate::Gate;
use axum::Router;
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::routing::{get, post};
use rahi_edge::{AppState, StreamRoutes};
use rahi_idp::{Bearer, BearerRoutes};
use rahi_types::{Error, Principal, Sub};

use crate::error::ApiError;
use crate::events::Events;
use crate::scopes::{ApiScope, scoped};
use crate::{capture, events};

/// Where the router is mounted (B-1, B-10).
pub const API_PREFIX: &str = "/api/v1";

/// The promotion subtree, which a client credentials token never reaches
/// (B-3, FR-003, `rahi://025` B-4).
pub const PROMOTE_PREFIX: &str = "/api/v1/promote";

/// The default ceiling on candidates per capture (B-9).
pub const DEFAULT_MAX_BATCH: usize = 100;

/// How long an `Idempotency-Key` is remembered, in seconds (B-8, D-7).
pub const DEFAULT_IDEMPOTENCY_TTL_SECS: u32 = 24 * 60 * 60;

/// Where this surface accepts a bearer credential (B-3, D-5).
///
/// The whole surface is service-callable, so an importer or the orchestrator
/// holding a client credentials token reaches capture and the listing; the
/// promotion subtree is declared for a person's token only, so the chassis
/// refuses a client credentials token there before the handler runs. The
/// longest matching prefix wins.
#[must_use]
pub fn bearer_routes() -> BearerRoutes {
    BearerRoutes::new()
        .service_route(API_PREFIX)
        .route(PROMOTE_PREFIX)
}

/// The tunable limits of this surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Candidates per capture (B-9).
    pub max_batch: usize,
    /// How long an idempotency key is remembered, in seconds (B-8).
    pub idempotency_ttl_secs: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_batch: DEFAULT_MAX_BATCH,
            idempotency_ttl_secs: DEFAULT_IDEMPOTENCY_TTL_SECS,
        }
    }
}

/// What every handler shares.
#[derive(Clone)]
pub struct Api {
    /// The chassis's state: store, ledger, kernel, configuration.
    pub app: AppState,
    /// The write gate (013).
    pub gate: Arc<Gate>,
    /// The node's event bus (B-6).
    pub events: Events,
    /// The surface's limits.
    pub limits: Limits,
}

impl std::fmt::Debug for Api {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Api")
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

impl Api {
    /// The surface over the chassis's state, with the shipped gate and limits.
    #[must_use]
    pub fn new(app: AppState) -> Self {
        Self {
            app,
            gate: Arc::new(Gate::standard()),
            events: Events::new(),
            limits: Limits::default(),
        }
    }

    /// The same, with other limits.
    #[must_use]
    pub const fn with_limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }
}

/// Who is asking: the principal the chassis resolved, and the credential.
///
/// Every route is behind a scope gate, which refuses a request with no
/// principal, so a handler that extracts this has one. A request that
/// arrives here without one is answered 401 rather than trusted.
#[derive(Clone, Debug)]
pub struct Caller {
    principal: Principal,
    bearer: Option<Bearer>,
}

impl Caller {
    /// The subject: the only identity (constitution VII).
    #[must_use]
    pub const fn sub(&self) -> &Sub {
        &self.principal.sub
    }

    /// The client that presented the token, when a token was presented.
    #[must_use]
    pub fn client_id(&self) -> Option<&str> {
        self.bearer.as_ref().map(|bearer| bearer.client_id.as_str())
    }

    /// Whether the subject is a client acting as itself (`rahi://025` B-4).
    #[must_use]
    pub fn is_service(&self) -> bool {
        self.bearer.as_ref().is_some_and(|bearer| bearer.service)
    }
}

impl<S: Send + Sync> FromRequestParts<S> for Caller {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let principal = parts
            .extensions
            .get::<Principal>()
            .cloned()
            .ok_or_else(|| {
                ApiError::from(Error::Unauthorized(
                    "this request carries no credential".to_owned(),
                ))
            })?;
        Ok(Self {
            principal,
            bearer: parts.extensions.get::<Bearer>().cloned(),
        })
    }
}

/// The product router, mounted under [`API_PREFIX`] (B-1, B-4).
///
/// Staged per D-3: `POST /search` arrives with 018 and the entity routes with
/// 017, each mounted here through an `extends` edge on this file. The memory
/// routes of B-4 land in this spec's second change.
pub fn router(state: AppState) -> Router {
    router_with(Api::new(state))
}

/// [`router`] over an explicitly built [`Api`].
pub fn router_with(api: Api) -> Router {
    let kernel = api.app.kernel().clone();
    let k = &kernel;
    let routes = Router::new()
        .route(
            "/capture",
            scoped(ApiScope::MemoryWrite, k, post(capture::capture)),
        )
        .stream_route(
            "/events",
            scoped(ApiScope::MemoryRead, k, get(events::events)),
        )
        .with_state(api);
    Router::new().nest(API_PREFIX, routes)
}
