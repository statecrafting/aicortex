//! The graph preflight check (spec 017 AC-2).
//!
//! `aicortex preflight` runs this after the chassis's own checks and prints
//! it as `app.graph`. It writes nothing and opens no socket: it reads entity
//! and active edge counts per scope, and the extraction queue's pending and
//! dead-letter counts, from the chassis's read-only store view. A non-zero
//! dead-letter count or an open contradiction is a warning.

#![forbid(unsafe_code)]

use std::time::Duration;

use aicortex_graph::{GraphPreflight, read_deployment};
use aicortex_store::ReadStore;
use rahi_cli::{AppCheck, AppVerdict, PreflightContext};

use crate::embedding_preflight::ViewStore;

/// The check's name; the chassis prints it as `app.graph`.
pub const CHECK_NAME: &str = "graph";

/// How long the store walk may take; the chassis bounds one check at five
/// seconds, so the walk reports a partial figure rather than being cut off.
const READ_BUDGET: Duration = Duration::from_secs(3);

/// The check the cell declares.
#[must_use]
pub fn check() -> AppCheck {
    AppCheck::new(CHECK_NAME, |context: PreflightContext| async move {
        let store = ViewStore::new(context.store().clone());
        verdict(&store, READ_BUDGET).await
    })
}

/// The check's verdict for a store.
pub async fn verdict(store: &impl ReadStore, budget: Duration) -> AppVerdict {
    let health = match read_deployment(store, budget).await {
        Ok(health) => health,
        Err(error) => return AppVerdict::fail(format!("cannot read graph state: {error}")),
    };
    let report = lines(&health);
    let contradictions: u64 = health.scopes.iter().map(|s| s.open_contradictions).sum();
    let summary = format!(
        "{} entities, {} edges, {} extraction dead letters across {} scope(s)",
        health.entities(),
        health.edges(),
        health.dead(),
        health.scopes.len()
    );
    let verdict = if health.dead() > 0 {
        AppVerdict::warn(format!(
            "{} dead extraction job(s) require operator attention; {summary}",
            health.dead()
        ))
    } else if contradictions > 0 {
        AppVerdict::warn(format!(
            "{contradictions} open contradiction(s) await review; {summary}"
        ))
    } else if !health.complete {
        AppVerdict::warn(format!("graph state is partial; {summary}"))
    } else {
        AppVerdict::pass(summary)
    };
    verdict.with_report(report)
}

fn lines(health: &GraphPreflight) -> Vec<String> {
    let mut report = Vec::new();
    for scope in &health.scopes {
        report.push(format!(
            "scope {}: entities={} edges={} contradictions={} rejected_edges={}",
            scope.scope_id,
            scope.entities,
            scope.edges,
            scope.open_contradictions,
            scope.rejected_edges
        ));
    }
    report.push(format!(
        "extraction queue: pending={} dead={} quarantined={}",
        health.scopes.iter().map(|s| s.pending).sum::<u64>(),
        health.dead(),
        health.scopes.iter().map(|s| s.quarantined).sum::<u64>()
    ));
    report.push(format!(
        "scopes read: {}{}",
        health.scopes.len(),
        if health.complete {
            ""
        } else {
            " (time budget reached; figures are a lower bound)"
        }
    ));
    report
}
