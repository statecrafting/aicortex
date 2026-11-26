//! The aicortex cell (spec 010 B-3): the manifest, migrations, and routers
//! the chassis mounts, plus product checks that extend chassis operations.
//!
//! Later specs extend this file additively: a crate that owns schema adds
//! its migrations to [`Cell::migrations`], and a surface crate merges its
//! router into [`Cell::routes`]. Each such spec declares an `extends` edge
//! on this file.

#![forbid(unsafe_code)]

mod embedding;
pub mod embedding_fetch;
pub mod embedding_preflight;
pub mod embedding_service;
pub mod graph_preflight;
pub mod graph_service;

use std::sync::OnceLock;

use aicortex_api::{Api, Events};
use axum::Router;
use rahi_cli::{AppCheck, Cell, ManagedService, ServiceShutdown};
use rahi_edge::AppState;
use rahi_idp::BearerRoutes;
use rahi_store::{Migration, MigrationSet};
use rahi_types::Result;

/// The node's event bus (spec 020 B-6, D-15).
///
/// `routes` and `services` are built apart from one `AppState` the chassis
/// owns, so the bus that `GET /events` subscribes to and the one the embedding
/// worker publishes to are joined here, once per process. It carries ids,
/// codes, and states, and no store handle (B-5, D-9).
#[must_use]
pub fn events() -> Events {
    static BUS: OnceLock<Events> = OnceLock::new();
    BUS.get_or_init(Events::new).clone()
}

/// The cell.
///
/// It holds no state. The chassis builds one [`AppState`] (the store, the
/// ledger, the kernel) and hands it to the routers, which clone it into
/// their handlers; no handle lives in a global (B-5 as amended, D-9).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Aicortex;

impl Aicortex {
    /// The capability ceiling, embedded at build (B-4).
    pub const MANIFEST: &'static str = include_str!("../manifest.toml");
}

impl Cell for Aicortex {
    fn manifest() -> &'static str {
        Self::MANIFEST
    }

    /// Every crate that owns schema contributes here, in version order.
    ///
    /// `aicortex-store` owns the memory schema (spec 012 B-1): the chassis's
    /// coordination tables at version 1, because a capture stages outbox work
    /// in its own transaction, and the five memory tables at version 2. A
    /// later spec that owns a table appends to that crate's list rather than
    /// to this one, so this seam stays a single call.
    fn migrations() -> &'static [Migration] {
        aicortex_store::migrations()
    }

    fn migration_sets() -> Vec<MigrationSet> {
        vec![rahi_store::coordination_set(), rahi_store::receipt_set()]
    }

    /// The merged product router: spec 020's HTTP API under `/api/v1` and
    /// spec 021's MCP server at `/mcp`, over one event bus.
    fn routes(state: AppState) -> Router {
        let api = Api::new(state).with_events(events());
        aicortex_api::router_with(api.clone()).merge(aicortex_mcp::router(api))
    }

    /// Where a bearer credential is accepted (spec 020 B-3, D-5): the whole
    /// API, service-callable except the promotion subtree, and the MCP
    /// endpoint (spec 021 B-4).
    fn bearer_routes() -> BearerRoutes {
        aicortex_mcp::bearer_routes(aicortex_api::bearer_routes())
    }

    /// The operator surface, mounted by the chassis behind the operator
    /// role. Spec 015 adds the embedding verbs (activate, re-embed, drop,
    /// status); the provider configuration is read from the process
    /// environment once, here.
    fn operator_routes(state: AppState) -> Router {
        let embedding = embedding::Embedding::new(state, &rahi_cli::process_env());
        embedding::mount(Router::new()).with_state(embedding)
    }

    /// The embedding preflight check (spec 015 B-3, AC-2, FR-005): the
    /// configured provider against the manifest ceiling, then the active
    /// revision, queue health, and coverage from the read-only store view;
    /// and the graph check (spec 017 AC-2): entity and edge counts per scope
    /// and the extraction dead-letter count.
    fn preflight_checks() -> Vec<AppCheck> {
        vec![embedding_preflight::check(), graph_preflight::check()]
    }

    /// The embedding worker and its metrics collector as managed services
    /// (spec 015 B-2, D-25), then the extraction worker (spec 017 B-7). A
    /// malformed or un-admitted provider configuration fails the composition,
    /// so `serve` refuses to start rather than failing at first use.
    fn services(state: AppState, shutdown: ServiceShutdown) -> Result<Vec<ManagedService>> {
        let mut services = embedding_service::compose(&state, &shutdown, &rahi_cli::process_env())?;
        services.extend(graph_service::compose(&state, &shutdown)?);
        Ok(services)
    }
}
