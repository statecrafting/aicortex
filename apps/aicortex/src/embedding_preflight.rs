//! The embedding preflight check (spec 015 B-3, AC-2, FR-003, FR-005).
//!
//! `aicortex preflight` runs this after the chassis's own checks and prints
//! it as `app.embedding`. It answers two questions without opening a socket
//! or writing a row:
//!
//! - Would the configured provider start here? The configuration is read
//!   from the verb's environment and held to the manifest ceiling, so a
//!   remote provider whose host is absent from the ceiling fails with the
//!   named capability error (FR-005).
//! - What state is the pipeline in? The active model revision, the pending
//!   and dead-letter counts, the oldest pending age, and coverage per
//!   revision (AC-2). A non-zero dead-letter count is a warning (B-3).
//!
//! The store is the chassis's read-only view. The figures are deployment
//! wide sums of scoped reads (D-28).

#![forbid(unsafe_code)]

use std::future::Future;
use std::path::Path;
use std::time::Duration;

use aicortex_embed::{EmbeddingConfig, EmbeddingPreflight, ModelId, unix_now};
use aicortex_store::ReadStore;
use rahi_cli::{AppCheck, AppVerdict, PreflightContext, StoreView};
use rahi_kernel::Manifest;
use rahi_store::Value;
use rahi_types::{EnvReader, Error, UnixSeconds};
use serde::de::DeserializeOwned;

/// The check's name; the chassis prints it as `app.embedding`.
pub const CHECK_NAME: &str = "embedding";

/// How long the store walk may take. The chassis bounds one check at five
/// seconds, so the walk ends well inside it and reports a partial figure
/// rather than being cut off silently.
const READ_BUDGET: Duration = Duration::from_secs(3);

/// The chassis's read-only store view, as a [`ReadStore`].
///
/// A newtype, because the trait and the view live in other crates.
#[derive(Clone, Debug)]
pub struct ViewStore(StoreView);

impl ViewStore {
    /// Wrap the view a preflight check was given.
    #[must_use]
    pub const fn new(view: StoreView) -> Self {
        Self(view)
    }
}

impl ReadStore for ViewStore {
    fn query<T: DeserializeOwned + Send + 'static>(
        &self,
        sql: &'static str,
        values: Vec<Value>,
    ) -> impl Future<Output = Result<Vec<T>, Error>> + Send {
        self.0.query(sql, values)
    }

    fn query_consistent<T: DeserializeOwned + Send + 'static>(
        &self,
        sql: &'static str,
        values: Vec<Value>,
    ) -> impl Future<Output = Result<Vec<T>, Error>> + Send {
        self.0.query_consistent(sql, values)
    }
}

/// The check the cell declares.
#[must_use]
pub fn check() -> AppCheck {
    AppCheck::new(CHECK_NAME, |context: PreflightContext| async move {
        let store = ViewStore::new(context.store().clone());
        verdict(
            context.env(),
            context.manifest(),
            &store,
            unix_now(),
            READ_BUDGET,
        )
        .await
    })
}

/// The check's verdict for an environment, a manifest, and a store.
pub async fn verdict<E: EnvReader + Sync>(
    env: &E,
    manifest: &Manifest,
    store: &impl ReadStore,
    now: UnixSeconds,
    budget: Duration,
) -> AppVerdict {
    let config = match EmbeddingConfig::from_env(env) {
        Ok(config) => config,
        Err(error) => return AppVerdict::fail(format!("embedding configuration: {error}")),
    };
    if let Err(error) = config.check(manifest) {
        return AppVerdict::fail(error.to_string());
    }
    let mut report = vec![format!("provider: {}", describe(&config))];
    let absent = absent_artifacts(&config);
    for path in &absent {
        report.push(format!(
            "artifact absent: {path} (supply it; this build links no fetch transport)"
        ));
    }
    let health = match EmbeddingPreflight::read_deployment(store, now, budget).await {
        Ok(health) => health,
        Err(error) => {
            return AppVerdict::fail(format!("cannot read embedding state: {error}"))
                .with_report(report);
        }
    };
    let state = &health.report;
    report.push(match &state.active {
        Some(model) => format!(
            "active model: {}@{} dims={} normalized={}",
            model.model_id.as_str(),
            model.revision,
            model.dims,
            model.normalized
        ),
        None => "active model: none".to_owned(),
    });
    let oldest = state.queue.oldest_pending_age_seconds.map_or_else(
        || {
            if state.queue.pending == 0 {
                "none".to_owned()
            } else {
                "unknown".to_owned()
            }
        },
        |age| age.to_string(),
    );
    report.push(format!(
        "queue: pending={} dead={} quarantined={} oldest_pending_seconds={oldest}",
        state.queue.pending, state.queue.dead, state.queue.quarantined
    ));
    report.push(format!("live memories: {}", state.live_memories));
    if state.coverage.is_empty() {
        report.push("coverage: no model revision recorded".to_owned());
    }
    for item in &state.coverage {
        report.push(format!(
            "coverage revision {}: {}/{}",
            item.revision, item.embedded, item.total
        ));
    }
    report.push(format!(
        "scopes read: {}{}",
        health.scopes_read,
        if health.complete {
            ""
        } else {
            " (time budget reached; figures are a lower bound)"
        }
    ));
    let summary = state.scope_summary().to_string();
    let verdict = if state.queue.dead > 0 {
        AppVerdict::warn(format!(
            "{} dead embedding work item(s) require operator attention; {summary}",
            state.queue.dead
        ))
    } else if let Some(warning) = state.readiness_warning() {
        AppVerdict::warn(format!("{warning}; {summary}"))
    } else if !health.complete {
        AppVerdict::warn(format!("embedding state is partial; {summary}"))
    } else if !absent.is_empty() {
        AppVerdict::warn(format!("model artifacts are absent; {summary}"))
    } else {
        AppVerdict::pass(summary)
    };
    verdict.with_report(report)
}

fn describe(config: &EmbeddingConfig) -> String {
    let model = |id: &ModelId| id.as_str().to_owned();
    match config {
        EmbeddingConfig::Disabled => "off (captures stage no embedding work)".to_owned(),
        EmbeddingConfig::Local(local) => format!("local {}", model(&local.shape.id)),
        EmbeddingConfig::Remote(remote) => format!("remote {}", model(&remote.shape.id)),
    }
}

/// Artifact paths of a local configuration that do not exist on disk.
fn absent_artifacts(config: &EmbeddingConfig) -> Vec<String> {
    let EmbeddingConfig::Local(local) = config else {
        return Vec::new();
    };
    [&local.weights, &local.tokenizer]
        .into_iter()
        .filter(|artifact| !Path::new(&artifact.path).exists())
        .map(|artifact| artifact.path.display().to_string())
        .collect()
}
