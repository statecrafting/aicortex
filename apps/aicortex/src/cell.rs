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

use axum::Router;
use rahi_cli::{AppCheck, Cell, ManagedService, ServiceShutdown};
use rahi_edge::AppState;
use rahi_store::{Migration, MigrationSet};
use rahi_types::Result;

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

    /// The merged product router. No product route exists yet.
    fn routes(state: AppState) -> Router {
        let _ = state;
        Router::new()
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
    /// revision, queue health, and coverage from the read-only store view.
    fn preflight_checks() -> Vec<AppCheck> {
        vec![embedding_preflight::check()]
    }

    /// The embedding worker and its metrics collector as managed services
    /// (spec 015 B-2, D-25). A malformed or un-admitted provider configuration
    /// fails the composition, so `serve` refuses to start rather than failing
    /// at first use.
    fn services(state: AppState, shutdown: ServiceShutdown) -> Result<Vec<ManagedService>> {
        embedding_service::compose(&state, &shutdown, &rahi_cli::process_env())
    }
}
