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
use aicortex_store::CursorKey;
use axum::Router;
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::routing::{delete, get, patch, post};
use rahi_edge::{AppState, StreamRoutes};
use rahi_idp::{Bearer, BearerRoutes};
use rahi_types::{Error, Principal, Sub};
use ring::rand::{SecureRandom, SystemRandom};
use tokio::sync::OnceCell;

use crate::error::ApiError;
use crate::events::Events;
use crate::scopes::{ApiScope, scoped};
use crate::{capture, events, memories};

/// Where the router is mounted (B-1, B-10).
pub const API_PREFIX: &str = "/api/v1";

/// The promotion subtree, which a client credentials token never reaches
/// (B-3, FR-003, `rahi://025` B-4).
pub const PROMOTE_PREFIX: &str = "/api/v1/promote";

/// The default ceiling on candidates per capture (B-9).
pub const DEFAULT_MAX_BATCH: usize = 100;

/// How long an `Idempotency-Key` is remembered, in seconds (B-8, D-7).
pub const DEFAULT_IDEMPOTENCY_TTL_SECS: u32 = 24 * 60 * 60;

/// The cache key the listing cursor's signing secret lives under (D-11).
pub const CURSOR_KEY_CACHE_KEY: &str = "aicortex.api.cursor-key";

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
    cursor: Arc<OnceCell<CursorKey>>,
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
            cursor: Arc::new(OnceCell::new()),
        }
    }

    /// The same, with other limits.
    #[must_use]
    pub const fn with_limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    /// The key the listing cursor is signed with (D-11).
    ///
    /// Kept in the chassis's replicated cache so every node signs and checks
    /// with the same secret. The first node to need one writes a random key
    /// and every node then reads back whichever write won, so a race costs at
    /// most the cursors signed in that instant. The cache is derived state:
    /// if it is cleared, outstanding cursors stop verifying and a client
    /// starts its listing again, and no decision depends on that.
    ///
    /// # Errors
    ///
    /// The store's error, or [`Error::Integrity`] when no entropy is
    /// available.
    pub async fn cursor_key(&self) -> Result<&CursorKey, Error> {
        self.cursor
            .get_or_try_init(|| async {
                let store = self.app.store();
                if let Some(hex) = store.kv_get::<String>(CURSOR_KEY_CACHE_KEY).await? {
                    return Ok(CursorKey::from_secret(hex.as_bytes()));
                }
                let mut secret = [0_u8; CursorKey::BYTES];
                SystemRandom::new()
                    .fill(&mut secret)
                    .map_err(|_| Error::Integrity("no entropy for the cursor key".to_owned()))?;
                let hex = crate::capture::hex(&secret);
                store.kv_put(CURSOR_KEY_CACHE_KEY, &hex, None).await?;
                let winner = store
                    .kv_get::<String>(CURSOR_KEY_CACHE_KEY)
                    .await?
                    .unwrap_or(hex);
                Ok(CursorKey::from_secret(winner.as_bytes()))
            })
            .await
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
/// 017, each mounted here through an `extends` edge on this file.
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
        .route(
            "/memories",
            scoped(ApiScope::MemoryRead, k, get(memories::list)),
        )
        .route(
            "/memories/{id}",
            scoped(ApiScope::MemoryRead, k, get(memories::get_one))
                .merge(scoped(ApiScope::MemoryWrite, k, patch(memories::patch_one)))
                .merge(scoped(ApiScope::MemoryDelete, k, delete(memories::erase))),
        )
        .route(
            "/stats",
            scoped(ApiScope::MemoryRead, k, get(memories::stats)),
        )
        .route(
            "/promote/{id}",
            scoped(ApiScope::MemoryPromote, k, post(memories::promote)),
        )
        .stream_route(
            "/events",
            scoped(ApiScope::MemoryRead, k, get(events::events)),
        )
        .with_state(api);
    Router::new().nest(API_PREFIX, routes)
}
