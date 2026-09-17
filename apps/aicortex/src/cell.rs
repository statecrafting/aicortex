//! The aicortex cell (spec 010 B-3): the manifest, the migrations, and the
//! two routers the chassis mounts. Nothing else lives at this seam.
//!
//! Later specs extend this file additively: a crate that owns schema adds
//! its migrations to [`Cell::migrations`], and a surface crate merges its
//! router into [`Cell::routes`]. Each such spec declares an `extends` edge
//! on this file.

#![forbid(unsafe_code)]

use axum::Router;
use rahi_cli::Cell;
use rahi_edge::AppState;
use rahi_store::Migration;

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
    /// None exists yet, so the list is empty and the schema version is 0.
    fn migrations() -> &'static [Migration] {
        &[]
    }

    /// The merged product router. No product route exists yet.
    fn routes(state: AppState) -> Router {
        let _ = state;
        Router::new()
    }

    /// The operator surface, mounted by the chassis behind the operator
    /// role. Empty until a spec adds an operator route.
    fn operator_routes(state: AppState) -> Router {
        let _ = state;
        Router::new()
    }
}
