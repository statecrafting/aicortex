//! The run routes (spec 030 B-9).
//!
//! `POST /sources/{id}/runs` starts a run, `GET /sources/{id}/runs/{run}`
//! reports it, and `DELETE` cancels it. Progress streams on `/events` (the
//! runner publishes there). Intake sits above the surfaces in spec 002's
//! layering, so these handlers use the API's caller, errors, scope types and
//! scope gate downward, and [`routes`] is what the app hands to the API's
//! extension seam (`Api::with_routes`, 020 D-20), which mounts them inside the
//! same prefix, auth layers and scope gates as the API's own (030 D-2).
//!
//! Starting and cancelling a run write memory, so both need `memory.write`;
//! reading a report needs `memory.read`. The scope is resolved before any
//! store read, so a scope the caller cannot reach is a 404 that says nothing
//! about whether it holds a run. A run is started in the caller's name: the
//! subject on the token is the run's principal, and a client credentials token
//! is its own subject, as it is for capture. Nothing a source returned reaches
//! a response except the probe's label, bounded and stripped of control
//! characters.

use aicortex_api::dto::{ScopeQuery, ScopeRef};
use aicortex_api::{Api, ApiError, ApiResult, ApiRoute, ApiScope, Caller};
use aicortex_types::Scope;
use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post};
use serde::{Deserialize, Serialize};

use crate::adapter::{MAX_OPTIONS_BYTES, SourceId};
use crate::mapping::RunId;
use crate::run::{CancelOutcome, RunLimits, RunReport, RunRequest, RunState, Runner, Runtime};

/// The most bytes a start request may carry.
pub const MAX_REQUEST_BYTES: usize = 32 * 1024;

/// The most characters of a probe's label a response carries.
pub const MAX_LABEL_CHARS: usize = 120;

/// The routes of this module, for the app to hand to `Api::with_routes`: each
/// path relative to the API prefix, with the scope that gates it.
#[must_use]
pub fn routes(runner: Runner) -> Vec<ApiRoute> {
    let start = {
        let runner = runner.clone();
        move |State(api): State<Api>, caller: Caller, Path(id): Path<String>, body: Bytes| {
            let runner = runner.clone();
            async move { start(&runner, &api, &caller, &id, &body).await }
        }
    };
    let report = {
        let runner = runner.clone();
        move |State(api): State<Api>,
              caller: Caller,
              Path((id, run)): Path<(String, String)>,
              scope: Result<Query<ScopeQuery>, QueryRejection>| {
            let runner = runner.clone();
            async move { report(&runner, &api, &caller, &id, &run, scope).await }
        }
    };
    let cancel = move |State(api): State<Api>,
                       caller: Caller,
                       Path((id, run)): Path<(String, String)>,
                       scope: Result<Query<ScopeQuery>, QueryRejection>| {
        let runner = runner.clone();
        async move { cancel(&runner, &api, &caller, &id, &run, scope).await }
    };
    vec![
        ApiRoute::new("/sources/{id}/runs", ApiScope::MemoryWrite, post(start)),
        ApiRoute::new(
            "/sources/{id}/runs/{run}",
            ApiScope::MemoryRead,
            get(report),
        ),
        ApiRoute::new(
            "/sources/{id}/runs/{run}",
            ApiScope::MemoryWrite,
            delete(cancel),
        ),
    ]
}

/// `POST /sources/{id}/runs`.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct StartBody {
    /// The scope the items go to; the caller's personal scope when absent.
    #[serde(default)]
    scope: Option<ScopeRef>,
    /// What the source is configured with.
    #[serde(default)]
    options: serde_json::Value,
    /// Map and gate without writing.
    #[serde(default)]
    dry_run: bool,
    /// The item budget.
    #[serde(default)]
    max_items: Option<u64>,
    /// The rate limit, in items per second.
    #[serde(default)]
    rate_per_second: Option<u32>,
}

/// What a start answers.
#[derive(Debug, Serialize)]
struct StartedResponse {
    run_id: String,
    source: String,
    state: RunState,
    dry_run: bool,
    /// Whether the run resumes from the cursor an earlier run left.
    resumes: bool,
    /// What the source called what it found.
    label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    estimated_items: Option<u64>,
}

/// What a cancel answers.
#[derive(Debug, Serialize)]
struct Cancelled {
    run_id: String,
    outcome: &'static str,
}

fn source_id(text: &str) -> ApiResult<SourceId> {
    SourceId::new(text).map_err(|_| ApiError::not_found())
}

fn run_id(text: &str) -> ApiResult<RunId> {
    RunId::parse(text).map_err(|_| ApiError::not_found())
}

fn runtime(api: &Api) -> Runtime {
    Runtime {
        store: api.app.store().clone(),
        ledger: api.app.ledger().clone(),
        gate: std::sync::Arc::clone(&api.gate),
        events: api.events.clone(),
    }
}

fn scope_of(query: Result<Query<ScopeQuery>, QueryRejection>, caller: &Caller) -> ApiResult<Scope> {
    let Query(query) = query.map_err(|rejection| ApiError::invalid(rejection.body_text()))?;
    query.scope_ref().resolve(caller.sub())
}

/// A source's label as a response carries it: bounded, and without control
/// characters, because it is text the source chose.
fn label(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .take(MAX_LABEL_CHARS)
        .collect()
}

async fn start(
    runner: &Runner,
    api: &Api,
    caller: &Caller,
    id: &str,
    body: &Bytes,
) -> ApiResult<(StatusCode, Json<StartedResponse>)> {
    let source = source_id(id)?;
    if body.len() > MAX_REQUEST_BYTES {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "request_too_large",
            format!("a start request is at most {MAX_REQUEST_BYTES} bytes"),
        ));
    }
    let request: StartBody = if body.is_empty() {
        StartBody::default()
    } else {
        serde_json::from_slice(body).map_err(|error| {
            ApiError::invalid(format!("the start request is malformed: {error}"))
        })?
    };
    let options_bytes =
        serde_json::to_vec(&request.options).map_or(usize::MAX, |bytes| bytes.len());
    if options_bytes > MAX_OPTIONS_BYTES {
        return Err(ApiError::invalid(format!(
            "options are at most {MAX_OPTIONS_BYTES} bytes"
        )));
    }
    let scope = request
        .scope
        .unwrap_or_else(ScopeRef::personal)
        .resolve(caller.sub())?;
    let defaults = RunLimits::default();
    let limits = RunLimits {
        max_items: request.max_items.unwrap_or(defaults.max_items),
        rate_per_second: request.rate_per_second.unwrap_or(defaults.rate_per_second),
        ..defaults
    };

    let runtime = runtime(api);
    let accepted = runner
        .start(
            &runtime,
            RunRequest {
                scope,
                source,
                principal: caller.sub().clone(),
                options: request.options,
                limits,
                dry_run: request.dry_run,
            },
        )
        .await?;
    let answer = StartedResponse {
        run_id: accepted.run_id().to_string(),
        source: id.to_owned(),
        state: RunState::Running,
        dry_run: accepted.is_dry_run(),
        resumes: accepted.resumes(),
        label: label(&accepted.probe().label),
        estimated_items: accepted.probe().estimated_items,
    };
    let runner = runner.clone();
    tokio::spawn(async move {
        runner.drive(&runtime, accepted).await;
    });
    Ok((StatusCode::ACCEPTED, Json(answer)))
}

async fn report(
    runner: &Runner,
    api: &Api,
    caller: &Caller,
    id: &str,
    run: &str,
    scope: Result<Query<ScopeQuery>, QueryRejection>,
) -> ApiResult<Json<RunReport>> {
    let (source, run) = (source_id(id)?, run_id(run)?);
    let scope = scope_of(scope, caller)?;
    runner
        .report(api.app.store(), &scope, &source, &run)
        .await?
        .map(Json)
        .ok_or_else(ApiError::not_found)
}

async fn cancel(
    runner: &Runner,
    api: &Api,
    caller: &Caller,
    id: &str,
    run: &str,
    scope: Result<Query<ScopeQuery>, QueryRejection>,
) -> ApiResult<(StatusCode, Json<Cancelled>)> {
    let (source, run) = (source_id(id)?, run_id(run)?);
    let scope = scope_of(scope, caller)?;
    match runner
        .cancel(api.app.store(), &scope, &source, &run)
        .await?
    {
        CancelOutcome::Requested => Ok((
            StatusCode::ACCEPTED,
            Json(Cancelled {
                run_id: run.to_string(),
                outcome: "requested",
            }),
        )),
        CancelOutcome::Ended(state) => Err(ApiError::new(
            StatusCode::CONFLICT,
            "run_ended",
            format!("the run has already ended: {}", state.label()),
        )),
        CancelOutcome::Unknown => Err(ApiError::not_found()),
    }
}
