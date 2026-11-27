//! What `aicortex preflight` reports about intake (spec 030 AC-2): the last
//! run of each source in each scope, and whether it left a cursor to resume
//! from.
//!
//! The registered sources and their hosts come from the registry
//! ([`crate::Registry::describe`]); this module reads the store side, through
//! the chassis's read-only view. It writes nothing and holds no content: a
//! run's row is ids, a state, a code and counters.

use std::time::{Duration, Instant};

use aicortex_store::{ReadStore, scope_keys_after};
use rahi_store::Value;
use rahi_types::Error;
use serde::Deserialize;

use crate::run::{RunFailure, RunState};

const SCOPE_PAGE: u32 = 100;

/// A source's most recent run in one scope.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LastRun {
    /// The source.
    pub source: String,
    /// The run.
    pub run: String,
    /// Where it is, or how it ended.
    pub state: RunState,
    /// Why it failed, when it did.
    pub failure: Option<RunFailure>,
    /// When it started, in Unix seconds.
    pub started: u64,
    /// How many items it delivered.
    pub seen: u64,
    /// Whether a cursor is saved, so the next run resumes.
    pub resumable: bool,
}

/// One scope's intake state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScopeIngest {
    /// The store's scope id.
    pub scope_id: String,
    /// The last run of each source that has run here, in source order.
    pub runs: Vec<LastRun>,
}

/// Intake state across the deployment.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IngestPreflight {
    /// Scopes that have had a run, in scope id order.
    pub scopes: Vec<ScopeIngest>,
    /// Whether every scope was read inside the time budget.
    pub complete: bool,
}

impl IngestPreflight {
    /// The runs, across scopes, that failed.
    #[must_use]
    pub fn failed(&self) -> usize {
        self.scopes
            .iter()
            .flat_map(|scope| &scope.runs)
            .filter(|run| run.state == RunState::Failed)
            .count()
    }

    /// The runs, across scopes, that are still running.
    #[must_use]
    pub fn running(&self) -> usize {
        self.scopes
            .iter()
            .flat_map(|scope| &scope.runs)
            .filter(|run| run.state == RunState::Running)
            .count()
    }
}

#[derive(Debug, Deserialize)]
struct LastRunRow {
    source_id: String,
    run_id: String,
    state: String,
    error_code: Option<String>,
    started: i64,
    seen: i64,
    resumable: i64,
}

impl LastRunRow {
    fn into_last_run(self) -> Result<LastRun, Error> {
        let corrupt = |what: &str| Error::Integrity(format!("an ingest run row holds {what}"));
        Ok(LastRun {
            state: RunState::parse(&self.state).ok_or_else(|| corrupt("an unknown state"))?,
            failure: self
                .error_code
                .as_deref()
                .map(|code| RunFailure::parse(code).ok_or_else(|| corrupt("an unknown code")))
                .transpose()?,
            started: u64::try_from(self.started).map_err(|_| corrupt("a negative time"))?,
            seen: u64::try_from(self.seen).map_err(|_| corrupt("a negative count"))?,
            resumable: self.resumable == 1,
            source: self.source_id,
            run: self.run_id,
        })
    }
}

/// The most sources one scope's report lists.
const SOURCES_PER_SCOPE: i64 = 200;

/// One scope's last runs.
///
/// # Errors
///
/// Store errors or a row that does not read back.
pub async fn read_scope(store: &impl ReadStore, scope_id: &str) -> Result<ScopeIngest, Error> {
    let rows: Vec<LastRunRow> = store
        .query(
            "SELECT r.source_id AS source_id, r.run_id AS run_id, r.state AS state,
                    r.error_code AS error_code, r.started AS started, r.seen AS seen,
                    EXISTS (SELECT 1 FROM ingest_cursor c
                            WHERE c.scope_id = r.scope_id AND c.source_id = r.source_id)
                        AS resumable
             FROM ingest_run r
             WHERE r.scope_id = ?1
               AND r.run_id = (SELECT r2.run_id FROM ingest_run r2
                               WHERE r2.scope_id = r.scope_id AND r2.source_id = r.source_id
                               ORDER BY r2.started DESC, r2.run_id DESC LIMIT 1)
             ORDER BY r.source_id LIMIT ?2",
            vec![Value::from(scope_id), Value::Integer(SOURCES_PER_SCOPE)],
        )
        .await?;
    Ok(ScopeIngest {
        scope_id: scope_id.to_owned(),
        runs: rows
            .into_iter()
            .map(LastRunRow::into_last_run)
            .collect::<Result<_, _>>()?,
    })
}

/// Every scope's last runs, until `budget` is spent. A scope that has never
/// run a source is not listed.
///
/// # Errors
///
/// Store errors or a row that does not read back.
pub async fn read_deployment(
    store: &impl ReadStore,
    budget: Duration,
) -> Result<IngestPreflight, Error> {
    let started = Instant::now();
    let mut report = IngestPreflight {
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
            let scope = read_scope(store, scope_id).await?;
            if !scope.runs.is_empty() {
                report.scopes.push(scope);
            }
        }
        after = Some(last);
    }
    Ok(report)
}
