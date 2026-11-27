//! The runner's vocabulary and its public face (spec 030 B-2, B-3, B-8, B-10).
//!
//! One runner drives every adapter. [`Runner::start`] checks that a run can
//! happen and records it; [`Runner::drive`] pages, maps, judges and stages it
//! (in [`crate::drive`]); [`Runner::cancel`] and [`Runner::report`] are what
//! the routes call. A run is a row in `ingest_run` that every page's
//! transaction updates, so a run's state and counts are as durable as the
//! items they describe. A dry run writes no row at all (B-10): it lives on a
//! bounded in-memory board, so it is visible to the node that ran it and gone
//! with it (030 D-8).

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use aicortex_api::Events;
use aicortex_api::capture::now;
use aicortex_gate::Gate;
use aicortex_store::ScopeId;
use aicortex_types::Scope;
use rahi_kernel::{Egress, Governed};
use rahi_ledger::Ledger;
use rahi_store::{Statement, StoreHandle, TxnBuilder, Value};
use rahi_types::{Error, Sub, UnixSeconds};
use serde::{Deserialize, Serialize};

use crate::adapter::{AdapterContext, AdapterError, Probe, SourceId};
use crate::cursor::{Cursor, CursorRepo, seconds};
use crate::mapping::{IngestPolicy, RunId};
use crate::registry::{Registered, Registry};

/// The most items a run may be budgeted.
pub const MAX_ITEM_BUDGET: u64 = 10_000_000;

/// The fastest rate a run may declare, in items per second.
pub const MAX_RATE_PER_SECOND: u32 = 100_000;

/// The most times one page is retried before the run stops.
pub const MAX_RETRIES: u32 = 10;

/// The most items a dry run looks at: it remembers what it has seen to count a
/// repeat as the real run will, and that memory is bounded here (B-10).
pub const MAX_DRY_RUN_ITEMS: u64 = 100_000;

/// How many dry runs the board remembers.
pub const MAX_BOARD_RUNS: usize = 64;

/// How long a run may go without touching its row before another run of the
/// same source may take it over, as the default.
pub const DEFAULT_STALE_AFTER: Duration = Duration::from_secs(120);

/// Where a run is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    /// Paging.
    Running,
    /// The source had no more pages.
    Completed,
    /// The item budget ran out first; the cursor is kept for the next run.
    Exhausted,
    /// A person stopped it; the cursor is kept.
    Cancelled,
    /// It stopped with a recorded code; the cursor is kept.
    Failed,
}

impl RunState {
    /// Every state.
    pub const ALL: [Self; 5] = [
        Self::Running,
        Self::Completed,
        Self::Exhausted,
        Self::Cancelled,
        Self::Failed,
    ];

    /// The stable name the table and the events carry.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Exhausted => "exhausted",
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
        }
    }

    /// The state named by `text`.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|state| state.label() == text)
    }
}

/// Why a run stopped without finishing: a closed set of codes, because a run's
/// record never carries what a source said.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunFailure {
    /// The adapter answered with a failure that is not worth retrying.
    AdapterFailed,
    /// A page could not be fetched or committed after the retry ceiling.
    RetriesExhausted,
    /// The adapter returned a page the runner will not use.
    BadPage,
    /// The store refused what the runner staged.
    StoreFailed,
    /// Items committed, and a Decision for one of them could not be appended.
    LedgerUnavailable,
    /// Another run took this one's place, or its row is gone.
    Superseded,
    /// A later run found this one silent for too long and took it over.
    Abandoned,
}

impl RunFailure {
    /// Every code.
    pub const ALL: [Self; 7] = [
        Self::AdapterFailed,
        Self::RetriesExhausted,
        Self::BadPage,
        Self::StoreFailed,
        Self::LedgerUnavailable,
        Self::Superseded,
        Self::Abandoned,
    ];

    /// The stable name the table and the events carry.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::AdapterFailed => "adapter_failed",
            Self::RetriesExhausted => "retries_exhausted",
            Self::BadPage => "bad_page",
            Self::StoreFailed => "store_failed",
            Self::LedgerUnavailable => "ledger_unavailable",
            Self::Superseded => "superseded",
            Self::Abandoned => "abandoned",
        }
    }

    /// The code named by `text`.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|code| code.label() == text)
    }
}

/// What a run did, item by item (B-10).
///
/// Every item the adapter delivered lands in exactly one of the last six, so
/// `seen` is their sum. The same counts describe a dry run, which is what
/// lets a person see the shape of an import before committing to it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct RunCounts {
    /// Items delivered.
    pub seen: u64,
    /// New memories stored active.
    pub admitted: u64,
    /// Items whose external id was imported before: updated in place when the
    /// content is the same, superseded by the new content when it changed.
    pub updated: u64,
    /// New external ids whose content another memory already held (014 B-2).
    pub merged: u64,
    /// New memories stored quarantined.
    pub quarantined: u64,
    /// Items the gate refused: nothing stored.
    pub refused: u64,
    /// Items the adapter could not map.
    pub unmapped: u64,
}

impl RunCounts {
    pub(crate) const fn add(&mut self, other: &Self) {
        self.seen = self.seen.saturating_add(other.seen);
        self.admitted = self.admitted.saturating_add(other.admitted);
        self.updated = self.updated.saturating_add(other.updated);
        self.merged = self.merged.saturating_add(other.merged);
        self.quarantined = self.quarantined.saturating_add(other.quarantined);
        self.refused = self.refused.saturating_add(other.refused);
        self.unmapped = self.unmapped.saturating_add(other.unmapped);
    }
}

/// A run's rate and budget (B-8), enforced by the runner and never by an
/// adapter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RunLimits {
    /// The most items the run delivers before it stops.
    pub max_items: u64,
    /// The most items per second it takes in.
    pub rate_per_second: u32,
    /// How many times a failing page is retried.
    pub retries: u32,
    /// The wait before the first retry; each further one doubles it.
    pub backoff: Duration,
}

impl Default for RunLimits {
    fn default() -> Self {
        Self {
            max_items: 100_000,
            rate_per_second: 50,
            retries: 3,
            backoff: Duration::from_millis(250),
        }
    }
}

impl RunLimits {
    /// The limits, if they are inside the ceilings.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] naming the limit that is zero or over its
    /// ceiling.
    pub fn validated(self) -> Result<Self, Error> {
        if self.max_items == 0 || self.max_items > MAX_ITEM_BUDGET {
            return Err(Error::Validation(format!(
                "an item budget is 1 to {MAX_ITEM_BUDGET}, not {}",
                self.max_items
            )));
        }
        if self.rate_per_second == 0 || self.rate_per_second > MAX_RATE_PER_SECOND {
            return Err(Error::Validation(format!(
                "a rate is 1 to {MAX_RATE_PER_SECOND} items per second, not {}",
                self.rate_per_second
            )));
        }
        if self.retries > MAX_RETRIES {
            return Err(Error::Validation(format!(
                "a retry ceiling is at most {MAX_RETRIES}, not {}",
                self.retries
            )));
        }
        Ok(self)
    }
}

/// What starts a run.
#[derive(Clone, Debug)]
pub struct RunRequest {
    /// Where the items go.
    pub scope: Scope,
    /// Which registered source.
    pub source: SourceId,
    /// The authenticated person or client who started it: the subject every
    /// Decision of the run names.
    pub principal: Sub,
    /// What the source is configured with, for the adapter alone.
    pub options: serde_json::Value,
    /// The run's rate and budget.
    pub limits: RunLimits,
    /// Map and gate without writing (B-10).
    pub dry_run: bool,
}

/// A run, as it is reported (B-9).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RunReport {
    /// The run.
    pub run_id: String,
    /// The source.
    pub source: String,
    /// Whether it wrote nothing.
    pub dry_run: bool,
    /// Where it is.
    pub state: RunState,
    /// Why it stopped, when it failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<RunFailure>,
    /// Whether a person asked it to stop.
    pub cancel_requested: bool,
    /// What it has done.
    pub counts: RunCounts,
    /// When it started, in Unix seconds.
    pub started: u64,
    /// When it ended, in Unix seconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished: Option<u64>,
}

/// What a cancellation found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CancelOutcome {
    /// The run was running and is asked to stop after its current page.
    Requested,
    /// The run had already ended in this state.
    Ended(RunState),
    /// No such run in this scope and source.
    Unknown,
}

/// The node-local record of dry runs (B-10).
#[derive(Debug, Default)]
pub(crate) struct Board {
    runs: Mutex<BTreeMap<String, BoardRun>>,
}

#[derive(Debug)]
struct BoardRun {
    scope_id: String,
    cancel: Arc<AtomicBool>,
    report: RunReport,
}

impl Board {
    fn with<T>(&self, op: impl FnOnce(&mut BTreeMap<String, BoardRun>) -> T) -> T {
        let mut runs = self.runs.lock().unwrap_or_else(PoisonError::into_inner);
        op(&mut runs)
    }

    /// Admit a new dry run, evicting the oldest ended one when full.
    fn admit(
        &self,
        scope_id: &str,
        report: RunReport,
        cancel: Arc<AtomicBool>,
    ) -> Result<(), Error> {
        self.with(|runs| {
            if runs.len() >= MAX_BOARD_RUNS {
                // Run ids are `run-` and a UUIDv7, whose text order is its
                // creation order, so the smallest id is the oldest run.
                let oldest = runs
                    .iter()
                    .filter(|(_, run)| run.report.state != RunState::Running)
                    .map(|(id, _)| id.clone())
                    .min();
                match oldest {
                    Some(id) => {
                        runs.remove(&id);
                    }
                    None => {
                        return Err(Error::Conflict(format!(
                            "{MAX_BOARD_RUNS} dry runs are still running on this node"
                        )));
                    }
                }
            }
            runs.insert(
                report.run_id.clone(),
                BoardRun {
                    scope_id: scope_id.to_owned(),
                    cancel,
                    report,
                },
            );
            Ok(())
        })
    }

    pub(crate) fn update(&self, report: &RunReport) {
        self.with(|runs| {
            if let Some(run) = runs.get_mut(&report.run_id) {
                run.report = report.clone();
            }
        });
    }

    fn get(&self, scope_id: &str, source: &SourceId, run: &RunId) -> Option<RunReport> {
        self.with(|runs| {
            runs.get(run.as_str())
                .filter(|found| {
                    found.scope_id == scope_id && found.report.source == source.as_str()
                })
                .map(|found| found.report.clone())
        })
    }

    fn cancel(&self, scope_id: &str, source: &SourceId, run: &RunId) -> Option<CancelOutcome> {
        self.with(|runs| {
            let found = runs.get_mut(run.as_str())?;
            if found.scope_id != scope_id || found.report.source != source.as_str() {
                return None;
            }
            if found.report.state != RunState::Running {
                return Some(CancelOutcome::Ended(found.report.state));
            }
            found.cancel.store(true, Ordering::SeqCst);
            found.report.cancel_requested = true;
            Some(CancelOutcome::Requested)
        })
    }
}

/// What a run needs from the cell, handed in per call so the runner holds no
/// handle of its own (010 B-5).
#[derive(Clone, Debug)]
pub struct Runtime {
    /// The store.
    pub store: StoreHandle,
    /// The decision chain.
    pub ledger: Ledger,
    /// The write gate.
    pub gate: Arc<Gate>,
    /// The event bus progress is published on.
    pub events: Events,
}

/// A run that has been accepted and not yet driven.
#[derive(Debug)]
pub struct Started {
    pub(crate) run: RunId,
    pub(crate) request: RunRequest,
    pub(crate) source: Registered,
    pub(crate) resume: Option<Cursor>,
    pub(crate) began: UnixSeconds,
    pub(crate) cancel: Arc<AtomicBool>,
    pub(crate) probe: Probe,
}

impl Started {
    /// The run's id.
    #[must_use]
    pub const fn run_id(&self) -> &RunId {
        &self.run
    }

    /// What the source said when probed.
    #[must_use]
    pub const fn probe(&self) -> &Probe {
        &self.probe
    }

    /// Whether this run writes nothing.
    #[must_use]
    pub const fn is_dry_run(&self) -> bool {
        self.request.dry_run
    }

    /// Whether this run resumes from a cursor an earlier run left.
    #[must_use]
    pub const fn resumes(&self) -> bool {
        self.resume.is_some()
    }

    pub(crate) fn report(&self, state: RunState, counts: RunCounts) -> RunReport {
        RunReport {
            run_id: self.run.to_string(),
            source: self.request.source.to_string(),
            dry_run: self.request.dry_run,
            state,
            failure: None,
            cancel_requested: false,
            counts,
            started: self.began.get(),
            finished: None,
        }
    }
}

/// The one runner (B-2).
///
/// Cheap to clone: the registry, the board, and the egress facade are shared.
#[derive(Clone, Debug)]
pub struct Runner {
    pub(crate) registry: Arc<Registry>,
    pub(crate) egress: Option<Governed<Egress>>,
    pub(crate) policy: IngestPolicy,
    pub(crate) board: Arc<Board>,
    pub(crate) stale_after: Duration,
}

impl Runner {
    /// A runner over `registry`.
    ///
    /// `egress` is the ingest service's facade, which an adapter's context
    /// reaches the network through; a cell with no network source passes none.
    #[must_use]
    pub fn new(
        registry: Arc<Registry>,
        egress: Option<Governed<Egress>>,
        policy: IngestPolicy,
    ) -> Self {
        Self {
            registry,
            egress,
            policy,
            board: Arc::new(Board::default()),
            stale_after: DEFAULT_STALE_AFTER,
        }
    }

    /// The same runner, taking over a silent run after `stale_after`.
    #[must_use]
    pub fn with_stale_after(mut self, stale_after: Duration) -> Self {
        self.stale_after = stale_after;
        self
    }

    /// The registered sources.
    #[must_use]
    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    pub(crate) fn context(
        &self,
        request: &RunRequest,
        source: &Registered,
        page_limit: usize,
    ) -> AdapterContext {
        AdapterContext::new(
            request.options.clone(),
            page_limit,
            source.hosts(),
            self.egress.clone(),
            request.principal.clone(),
        )
    }

    /// Accept a run: probe the source, and for a real run record it.
    ///
    /// A source that does not answer its probe starts nothing. A real run
    /// refuses to start while another run of the same source is active in the
    /// scope, unless that run has been silent past the staleness bound, in
    /// which case this one takes its place and marks it abandoned in the same
    /// transaction.
    ///
    /// # Errors
    ///
    /// [`Error::NotFound`] for a source that is not registered;
    /// [`Error::Validation`] for limits outside their ceilings;
    /// [`Error::Upstream`] when the probe fails; [`Error::Conflict`] when a
    /// run is already active; the store's error.
    pub async fn start(&self, runtime: &Runtime, request: RunRequest) -> Result<Started, Error> {
        let limits = request.limits.validated()?;
        if request.dry_run && limits.max_items > MAX_DRY_RUN_ITEMS {
            return Err(Error::Validation(format!(
                "a dry run looks at most {MAX_DRY_RUN_ITEMS} items, not {}",
                limits.max_items
            )));
        }
        let request = RunRequest { limits, ..request };
        let source = self
            .registry
            .get(&request.source)
            .cloned()
            .ok_or_else(|| Error::NotFound(format!("no source {}", request.source)))?;
        let run = RunId::mint();
        let began = now();
        let resume = CursorRepo::load(&runtime.store, &request.scope, &request.source).await?;
        let probe = source
            .adapter()
            .probe(&self.context(&request, &source, 1))
            .await
            .map_err(|error| match error {
                AdapterError::Transient(_) => {
                    Error::Upstream("the source did not answer its probe".to_owned())
                }
                AdapterError::Fatal(_) => {
                    Error::Upstream("the source refused its probe".to_owned())
                }
            })?;
        let started = Started {
            run,
            request,
            source,
            resume,
            began,
            cancel: Arc::new(AtomicBool::new(false)),
            probe,
        };

        let scope_id = ScopeId::of(&started.request.scope);
        if started.request.dry_run {
            self.board.admit(
                scope_id.as_str(),
                started.report(RunState::Running, RunCounts::default()),
                Arc::clone(&started.cancel),
            )?;
            return Ok(started);
        }
        self.record(runtime, &started, &scope_id).await?;
        Ok(started)
    }

    async fn record(
        &self,
        runtime: &Runtime,
        started: &Started,
        scope_id: &ScopeId,
    ) -> Result<(), Error> {
        let request = &started.request;
        let at = seconds(started.began);
        let stale = i64::try_from(self.stale_after.as_secs()).unwrap_or(i64::MAX);
        let active: Vec<ActiveRow> = runtime
            .store
            .query_consistent(
                "SELECT touched FROM ingest_run
                 WHERE scope_id = $1 AND source_id = $2 AND state = 'running'",
                vec![Value::from(scope_id), Value::from(request.source.as_str())],
            )
            .await?;
        if active
            .iter()
            .any(|row| row.touched.saturating_add(stale) > at)
        {
            return Err(Error::Conflict(format!(
                "a run of {} is already active in this scope",
                request.source
            )));
        }
        let mut txn = TxnBuilder::new();
        txn.push(Statement::with_params(
            "UPDATE ingest_run SET state = 'failed', error_code = ?1, finished = ?2, touched = ?2
             WHERE scope_id = ?3 AND source_id = ?4 AND state = 'running' AND touched + ?5 <= ?2",
            vec![
                Value::from(RunFailure::Abandoned.label()),
                Value::Integer(at),
                Value::from(scope_id),
                Value::from(request.source.as_str()),
                Value::Integer(stale),
            ],
        ));
        // The read above gives a clear message in the common case; the partial
        // unique index `ingest_run_active` is what makes the rule hold. Of two
        // starts that both passed the read, the second insert violates it, and
        // the store reports a constraint violation as `Error::Conflict`, so
        // that caller gets the same conflict and no row (D-8).
        txn.push(Statement::with_params(
            "INSERT INTO ingest_run
                (run_id, scope_id, source_id, started_by, state, item_budget, rate_per_second,
                 started, touched)
             VALUES (?1, ?2, ?3, ?4, 'running', ?5, ?6, ?7, ?7)",
            vec![
                Value::from(started.run.as_str()),
                Value::from(scope_id),
                Value::from(request.source.as_str()),
                Value::from(request.principal.as_str()),
                Value::Integer(i64::try_from(request.limits.max_items).unwrap_or(i64::MAX)),
                Value::from(request.limits.rate_per_second),
                Value::Integer(at),
            ],
        ));
        runtime.store.txn(txn.into_statements()).await?;
        Ok(())
    }

    /// Start a run and drive it to its end.
    ///
    /// # Errors
    ///
    /// Whatever [`Self::start`] refuses; a run that stops after it started
    /// is a report, not an error.
    pub async fn run(&self, runtime: &Runtime, request: RunRequest) -> Result<RunReport, Error> {
        let started = self.start(runtime, request).await?;
        Ok(self.drive(runtime, started).await)
    }

    /// Ask a run to stop after its current page (FR-006).
    ///
    /// A real run is flagged in its row, so the request reaches whichever node
    /// is driving it; the runner reads the flag before every page.
    ///
    /// # Errors
    ///
    /// The store's error.
    pub async fn cancel(
        &self,
        store: &StoreHandle,
        scope: &Scope,
        source: &SourceId,
        run: &RunId,
    ) -> Result<CancelOutcome, Error> {
        let scope_id = ScopeId::of(scope);
        if let Some(outcome) = self.board.cancel(scope_id.as_str(), source, run) {
            return Ok(outcome);
        }
        let results = store
            .txn(vec![Statement::with_params(
                "UPDATE ingest_run SET cancel_requested = 1
                 WHERE run_id = ?1 AND scope_id = ?2 AND source_id = ?3 AND state = 'running'",
                vec![
                    Value::from(run.as_str()),
                    Value::from(&scope_id),
                    Value::from(source.as_str()),
                ],
            )])
            .await?;
        if results
            .first()
            .is_some_and(|result| result.rows_affected == 1)
        {
            return Ok(CancelOutcome::Requested);
        }
        Ok(match self.report(store, scope, source, run).await? {
            Some(report) => CancelOutcome::Ended(report.state),
            None => CancelOutcome::Unknown,
        })
    }

    /// One run of one source in one scope, as it stands.
    ///
    /// `query`, the local replica: a report is a report (012 B-5), and the
    /// scope and the source are in the statement, so a run id from another
    /// scope reads as absent.
    ///
    /// # Errors
    ///
    /// The store's error; [`Error::Integrity`] for a row that does not read
    /// back.
    pub async fn report(
        &self,
        store: &StoreHandle,
        scope: &Scope,
        source: &SourceId,
        run: &RunId,
    ) -> Result<Option<RunReport>, Error> {
        let scope_id = ScopeId::of(scope);
        if let Some(report) = self.board.get(scope_id.as_str(), source, run) {
            return Ok(Some(report));
        }
        let rows: Vec<RunRow> = store
            .query(
                format!(
                    "SELECT {RUN_COLUMNS} FROM ingest_run
                     WHERE run_id = $1 AND scope_id = $2 AND source_id = $3"
                ),
                vec![
                    Value::from(run.as_str()),
                    Value::from(&scope_id),
                    Value::from(source.as_str()),
                ],
            )
            .await?;
        rows.into_iter().next().map(RunRow::into_report).transpose()
    }
}

#[derive(Debug, Deserialize)]
struct ActiveRow {
    touched: i64,
}

/// The columns [`RunRow`] reads.
pub(crate) const RUN_COLUMNS: &str = "run_id, source_id, state, cancel_requested, started, \
    finished, seen, admitted, updated, merged, quarantined, refused, unmapped, error_code";

/// One `ingest_run` row.
#[derive(Debug, Deserialize)]
pub(crate) struct RunRow {
    pub(crate) run_id: String,
    pub(crate) source_id: String,
    pub(crate) state: String,
    pub(crate) cancel_requested: i64,
    pub(crate) started: i64,
    pub(crate) finished: Option<i64>,
    pub(crate) seen: i64,
    pub(crate) admitted: i64,
    pub(crate) updated: i64,
    pub(crate) merged: i64,
    pub(crate) quarantined: i64,
    pub(crate) refused: i64,
    pub(crate) unmapped: i64,
    pub(crate) error_code: Option<String>,
}

impl RunRow {
    pub(crate) fn into_report(self) -> Result<RunReport, Error> {
        let unsigned = |value: i64| {
            u64::try_from(value)
                .map_err(|_| Error::Integrity("a run row holds a negative figure".to_owned()))
        };
        let state = RunState::parse(&self.state).ok_or_else(|| {
            Error::Integrity(format!(
                "a run row holds the unknown state {:?}",
                self.state
            ))
        })?;
        let failure = self
            .error_code
            .as_deref()
            .map(|code| {
                RunFailure::parse(code).ok_or_else(|| {
                    Error::Integrity(format!("a run row holds the unknown code {code:?}"))
                })
            })
            .transpose()?;
        Ok(RunReport {
            run_id: self.run_id,
            source: self.source_id,
            dry_run: false,
            state,
            failure,
            cancel_requested: self.cancel_requested == 1,
            counts: RunCounts {
                seen: unsigned(self.seen)?,
                admitted: unsigned(self.admitted)?,
                updated: unsigned(self.updated)?,
                merged: unsigned(self.merged)?,
                quarantined: unsigned(self.quarantined)?,
                refused: unsigned(self.refused)?,
                unmapped: unsigned(self.unmapped)?,
            },
            started: unsigned(self.started)?,
            finished: self.finished.map(unsigned).transpose()?,
        })
    }
}
