//! What `aicortex preflight` reports about the graph (spec 017 AC-2): entity
//! and edge counts per scope and the extraction dead-letter count.

use std::time::{Duration, Instant};

use aicortex_store::{
    EMBEDDING_NAMESPACE, EXTRACTION_PROCESSOR, ReadStore, embedding_queue_counts, scope_keys_after,
};
use rahi_store::Value;
use rahi_types::Error;
use serde::Deserialize;

const SCOPE_PAGE: u32 = 100;

/// The graph's state in one scope.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScopeGraph {
    /// The store's scope id.
    pub scope_id: String,
    /// Entities that are not merged into another.
    pub entities: u64,
    /// Active edges.
    pub edges: u64,
    /// Contradictions awaiting review.
    pub open_contradictions: u64,
    /// Edges the vocabulary or the floor refused, over every extraction.
    pub rejected_edges: u64,
    /// Extraction jobs pending, claimed, or retrying.
    pub pending: u64,
    /// Extraction jobs that exhausted their attempts.
    pub dead: u64,
    /// Extraction jobs held for quarantine review.
    pub quarantined: u64,
}

/// The graph's state across the deployment.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GraphPreflight {
    /// Per-scope figures, in scope id order.
    pub scopes: Vec<ScopeGraph>,
    /// Whether every scope was read inside the time budget.
    pub complete: bool,
}

impl GraphPreflight {
    /// Extraction dead letters across the scopes read.
    #[must_use]
    pub fn dead(&self) -> u64 {
        self.scopes.iter().map(|s| s.dead).sum()
    }

    /// Entities across the scopes read.
    #[must_use]
    pub fn entities(&self) -> u64 {
        self.scopes.iter().map(|s| s.entities).sum()
    }

    /// Active edges across the scopes read.
    #[must_use]
    pub fn edges(&self) -> u64 {
        self.scopes.iter().map(|s| s.edges).sum()
    }
}

#[derive(Debug, Deserialize)]
struct CountRow {
    count: i64,
}

async fn count(store: &impl ReadStore, sql: &'static str, scope_id: &str) -> Result<u64, Error> {
    let rows: Vec<CountRow> = store.query(sql, vec![Value::from(scope_id)]).await?;
    let row = rows
        .first()
        .ok_or_else(|| Error::Integrity("graph count returned no row".to_owned()))?;
    u64::try_from(row.count).map_err(|_| Error::Integrity("graph count is negative".to_owned()))
}

/// One scope's graph figures.
///
/// # Errors
///
/// Store errors or corrupt aggregates.
pub async fn read_scope(store: &impl ReadStore, scope_id: &str) -> Result<ScopeGraph, Error> {
    let offset = i64::try_from(EXTRACTION_PROCESSOR.len() + 3)
        .map_err(|_| Error::Integrity("processor prefix is too long".to_owned()))?;
    let queue = embedding_queue_counts(
        store,
        scope_id,
        EMBEDDING_NAMESPACE,
        &format!("{EXTRACTION_PROCESSOR}.r[0-9]*"),
        offset,
    )
    .await?;
    let non_negative = |value: i64| u64::try_from(value).unwrap_or(0);
    Ok(ScopeGraph {
        scope_id: scope_id.to_owned(),
        entities: count(
            store,
            "SELECT COUNT(*) AS count FROM entity WHERE scope_id = ?1 AND merged_into IS NULL",
            scope_id,
        )
        .await?,
        edges: count(
            store,
            "SELECT COUNT(*) AS count FROM edge WHERE scope_id = ?1 AND status = 'active'",
            scope_id,
        )
        .await?,
        open_contradictions: count(
            store,
            "SELECT COUNT(*) AS count FROM contradiction WHERE scope_id = ?1 AND status = 'open'",
            scope_id,
        )
        .await?,
        rejected_edges: count(
            store,
            "SELECT COALESCE(SUM(rejected), 0) AS count FROM extraction WHERE scope_id = ?1",
            scope_id,
        )
        .await?,
        pending: non_negative(queue.pending),
        dead: non_negative(queue.dead),
        quarantined: non_negative(queue.quarantined),
    })
}

/// Every scope's graph figures, until `budget` is spent.
///
/// # Errors
///
/// Store errors or corrupt aggregates.
pub async fn read_deployment(
    store: &impl ReadStore,
    budget: Duration,
) -> Result<GraphPreflight, Error> {
    let started = Instant::now();
    let mut report = GraphPreflight {
        scopes: Vec::new(),
        complete: true,
    };
    let mut after: Option<String> = None;
    'pages: loop {
        let page = scope_keys_after(store, after.as_deref(), SCOPE_PAGE).await?;
        let Some(last) = page.last().cloned() else {
            break;
        };
        for scope_id in &page {
            if started.elapsed() >= budget {
                report.complete = false;
                break 'pages;
            }
            report.scopes.push(read_scope(store, scope_id).await?);
        }
        after = Some(last);
    }
    Ok(report)
}
