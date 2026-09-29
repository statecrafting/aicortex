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
use rahi_store::{Migration, MigrationSet, Store};
use rahi_types::{Config, EnvReader, Error, UnixSeconds};
use serde::Deserialize;

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
    /// role. Empty until a spec adds an operator route.
    fn operator_routes(state: AppState) -> Router {
        let _ = state;
        Router::new()
    }
}

#[derive(Debug, Deserialize)]
struct ScopeRow {
    scope_id: String,
}

/// Append embedding state to the chassis preflight report.
///
/// This runs after the chassis preflight attempt and reacquires the chassis
/// cell gate before opening the store. If another process owns that gate, the
/// embedding check reports a skip instead of bypassing it. Otherwise it
/// reports every scope separately and leaves the process-wide model identity
/// visible even when the deployment contains no scopes yet.
#[must_use]
pub fn embedding_preflight(env: &dyn EnvReader) -> i32 {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("error: io: the embedding preflight runtime cannot be built: {error}");
            return 3;
        }
    };
    match runtime.block_on(read_embedding_preflight(env)) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("error: {error}");
            error.exit_code()
        }
    }
}

async fn read_embedding_preflight(env: &dyn EnvReader) -> Result<(), Error> {
    let config = Config::from_env(env)?;
    let _gate = match rahi_ops::cell_lock::gate(
        &config,
        rahi_ops::cell_lock::Entry::Store { may_attach: false },
    ) {
        Ok(gate) => gate,
        Err(error) => {
            println!("embedding: skipped because the chassis cell gate refused: {error}");
            return Ok(());
        }
    };
    let secrets = rahi_ops::KeySet::of(&config).store_secrets()?;
    let store_config = rahi_ops::store_config(&config, env, secrets)?;
    let store = Store::open(&store_config).await?;
    let result = report_embeddings(&store, UnixSeconds::new(unix_now())).await;
    let shutdown = store.shutdown().await;
    result.and(shutdown)
}

async fn report_embeddings(store: &Store, now: UnixSeconds) -> Result<(), Error> {
    let handle = store.handle();
    let migrations = handle.recorded_migrations().await?;
    if !migrations
        .iter()
        .any(|migration| migration.version == aicortex_store::EMBEDDING_MIGRATION_VERSION)
    {
        println!("embedding: unavailable until migrate");
        return Ok(());
    }
    let deployment = aicortex_embed::EmbeddingPreflight::read(&handle, "", now).await?;
    let active = deployment.active.as_ref().map_or_else(
        || "none".to_owned(),
        |model| format!("{}@{}", model.model_id.as_str(), model.revision),
    );
    let oldest = deployment.queue.oldest_pending_age_seconds.map_or_else(
        || {
            if deployment.queue.pending == 0 {
                "none".to_owned()
            } else {
                "unknown".to_owned()
            }
        },
        |age| age.to_string(),
    );
    print!(
        "embedding: deployment active={active} pending={} dead={} oldest_pending_seconds={oldest}",
        deployment.queue.pending, deployment.queue.dead
    );
    if let Some(warning) = deployment.readiness_warning() {
        print!(" warning={warning}");
    }
    println!();

    const PAGE: i64 = 100;
    let mut after = String::new();
    let mut reported = false;
    loop {
        let scopes: Vec<ScopeRow> = handle
            .query(
                "SELECT scope_id FROM scope WHERE scope_id > ?1 ORDER BY scope_id LIMIT ?2",
                vec![after.clone().into(), PAGE.into()],
            )
            .await?;
        if scopes.is_empty() {
            break;
        }
        for scope in &scopes {
            let coverage =
                aicortex_embed::ModelRegistry::coverage(&handle, &scope.scope_id).await?;
            println!(
                "embedding: scope={} coverage=[{}]",
                scope.scope_id,
                format_coverage(&coverage)
            );
            reported = true;
        }
        after = scopes
            .last()
            .map(|scope| scope.scope_id.clone())
            .unwrap_or(after);
        if scopes.len() < usize::try_from(PAGE).unwrap_or(usize::MAX) {
            break;
        }
    }
    if !reported {
        println!(
            "embedding: scope=none coverage=[{}]",
            format_coverage(&deployment.coverage)
        );
    }
    Ok(())
}

fn format_coverage(coverage: &[aicortex_embed::Coverage]) -> String {
    coverage
        .iter()
        .map(|item| format!("{}:{}/{}", item.revision, item.embedded, item.total))
        .collect::<Vec<_>>()
        .join(",")
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
