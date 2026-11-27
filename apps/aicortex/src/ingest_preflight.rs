//! The ingest preflight check (spec 030 AC-2).
//!
//! `aicortex preflight` runs this after the chassis's own checks and prints it
//! as `app.ingest`. It writes nothing and opens no socket: it lists the
//! registered sources and the hosts each declares, holds those hosts to the
//! manifest ceiling again (a ceiling narrowed since registration fails here,
//! before `serve` does), and reads the last run of each source in each scope
//! from the chassis's read-only store view. A failed run is a warning.

#![forbid(unsafe_code)]

use std::time::Duration;

use aicortex_ingest::preflight::{IngestPreflight, read_deployment};
use aicortex_ingest::{INGEST_SERVICE, Registry};
use aicortex_store::ReadStore;
use rahi_cli::{AppCheck, AppVerdict, PreflightContext};
use rahi_kernel::{CapabilityKind, Manifest, ServiceName};
use rahi_types::EnvReader;

use crate::embedding_preflight::ViewStore;
use crate::ingest_surface;

/// The check's name; the chassis prints it as `app.ingest`.
pub const CHECK_NAME: &str = "ingest";

/// How long the store walk may take; the chassis bounds one check at five
/// seconds, so the walk reports a partial figure rather than being cut off.
const READ_BUDGET: Duration = Duration::from_secs(3);

/// The check the cell declares.
#[must_use]
pub fn check() -> AppCheck {
    AppCheck::new(CHECK_NAME, |context: PreflightContext| async move {
        let store = ViewStore::new(context.store().clone());
        verdict(
            context.env(),
            context.manifest(),
            &ingest_surface::registry(),
            &store,
            READ_BUDGET,
        )
        .await
    })
}

/// The check's verdict for an environment, a manifest, a registry and a store.
pub async fn verdict<E: EnvReader + Sync + ?Sized>(
    env: &E,
    manifest: &Manifest,
    registry: &Registry,
    store: &impl ReadStore,
    budget: Duration,
) -> AppVerdict {
    if let Err(error) = ingest_surface::policy(env) {
        return AppVerdict::fail(error.to_string());
    }
    let mut report = sources(registry);
    let Ok(service) = ServiceName::parse(INGEST_SERVICE) else {
        return AppVerdict::fail("the ingest service name is not well formed").with_report(report);
    };
    let uncovered = registry.uncovered(manifest, &service);
    for (source, host) in &uncovered {
        report.push(format!("host not granted: source {source} reaches {host}"));
    }
    if let Some((_, host)) = uncovered.first() {
        return AppVerdict::capability_missing(&service, CapabilityKind::HttpEgress, host)
            .with_report(report);
    }
    let health = match read_deployment(store, budget).await {
        Ok(health) => health,
        Err(error) => {
            return AppVerdict::fail(format!("cannot read ingest state: {error}"))
                .with_report(report);
        }
    };
    report.extend(runs(&health));
    let summary = format!(
        "{} source(s) registered, {} scope(s) with runs, {} failed",
        registry.len(),
        health.scopes.len(),
        health.failed()
    );
    let verdict = if health.failed() > 0 {
        AppVerdict::warn(format!("the last run of a source failed; {summary}"))
    } else if !health.complete {
        AppVerdict::warn(format!("ingest state is partial; {summary}"))
    } else {
        AppVerdict::pass(summary)
    };
    verdict.with_report(report)
}

fn sources(registry: &Registry) -> Vec<String> {
    let mut lines = vec![format!("registered sources: {}", registry.len())];
    for source in registry.describe() {
        let hosts = if source.hosts.is_empty() {
            "none".to_owned()
        } else {
            source.hosts.join(", ")
        };
        lines.push(format!(
            "source {}: kind={} hosts={hosts}",
            source.id, source.kind
        ));
    }
    lines
}

fn runs(health: &IngestPreflight) -> Vec<String> {
    let mut lines = Vec::new();
    for scope in &health.scopes {
        for run in &scope.runs {
            let failure = run
                .failure
                .map_or_else(String::new, |code| format!(" ({})", code.label()));
            lines.push(format!(
                "scope {} source {}: last run {}{failure} seen={} resumable={}",
                scope.scope_id,
                run.source,
                run.state.label(),
                run.seen,
                run.resumable
            ));
        }
    }
    lines.push(format!(
        "scopes read: {}{}",
        health.scopes.len(),
        if health.complete {
            ""
        } else {
            " (time budget reached; figures are a lower bound)"
        }
    ));
    lines
}
