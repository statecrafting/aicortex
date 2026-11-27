//! The run routes over a socket (spec 030 B-9, FR-003, FR-006).
//!
//! Tokens are bearer tokens signed by the cell's test key, as 020's, 021's and
//! 023's tests use. The runner behind the routes is the real one, over a stub
//! source, against the node the harness boots.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

// The API's own harness, included by path so there is one (as 021's and 023's
// tests do).
#[path = "../../aicortex-api/tests/common/mod.rs"]
mod common;
// The stub source and its fixture.
#[path = "common/mod.rs"]
mod fixture;

use std::sync::Arc;
use std::time::Duration;

use aicortex_ingest::{IngestPolicy, SourceKind};
use common::Who;
use fixture::{Faults, Stub, fixture as items, runner};
use reqwest::{Method, StatusCode, header};
use serde_json::{Value as Json, json};

fn alice() -> Who {
    Who::person("sub-alice", &["memory.read", "memory.write"])
}

async fn send(
    cell: &common::Cell,
    method: Method,
    path: &str,
    who: &Who,
    body: Option<Json>,
) -> (StatusCode, Json) {
    let mut request = cell.request(method, path, Some(who));
    if let Some(body) = body {
        request = request
            .header(header::CONTENT_TYPE, "application/json")
            .body(body.to_string());
    }
    let response = request.send().await.expect("answered");
    let status = response.status();
    let text = response.text().await.expect("a body");
    let json = serde_json::from_str(&text).unwrap_or(Json::String(text));
    (status, json)
}

/// Poll a run until it leaves `running`.
async fn settled(cell: &common::Cell, who: &Who, path: &str) -> Json {
    for _ in 0..300 {
        let (status, body) = send(cell, Method::GET, path, who, None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        if body["state"] != "running" {
            return body;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("the run did not settle");
}

fn runs(source: &str) -> String {
    format!("/api/v1/sources/{source}/runs")
}

async fn cell_with(stub: &Arc<Stub>) -> common::Cell {
    common::boot_routed(aicortex_ingest::routes::routes(runner(
        std::slice::from_ref(stub),
        IngestPolicy::default(),
    )))
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_run_is_started_reported_and_streamed_by_the_routes() {
    let stub = Stub::new("notes", SourceKind::Authenticated, items(250));
    let cell = cell_with(&stub).await;
    let mut events = cell.events.subscribe();

    let (status, started) = send(
        &cell,
        Method::POST,
        &runs("notes"),
        &alice(),
        Some(json!({ "options": { "folder": "inbox" } })),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{started}");
    assert_eq!(started["state"], "running");
    assert_eq!(started["dry_run"], false);
    assert_eq!(started["label"], "fixture");
    assert_eq!(started["estimated_items"], 250);
    let run = started["run_id"].as_str().unwrap().to_owned();

    let report = settled(&cell, &alice(), &format!("{}/{run}", runs("notes"))).await;
    assert_eq!(report["state"], "completed", "{report}");
    assert_eq!(report["counts"]["seen"], 250);
    assert_eq!(report["counts"]["admitted"], 250);
    assert_eq!(cell.rows("memory").await, 250);

    // Progress was published for this person, as ids and states only.
    let mut states = Vec::new();
    while let Ok(event) = events.try_recv() {
        assert_eq!(event.name, "ingest");
        assert_eq!(event.data.id.as_deref(), Some(run.as_str()));
        states.push(event.data.state);
    }
    assert_eq!(states.last().map(String::as_str), Some("completed"));

    // A run that ended cannot be cancelled.
    let (status, body) = send(
        &cell,
        Method::DELETE,
        &format!("{}/{run}", runs("notes")),
        &alice(),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    cell.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dry_run_over_http_writes_nothing() {
    // FR-003 through the surface.
    let stub = Stub::new("notes", SourceKind::Authenticated, items(150));
    let cell = cell_with(&stub).await;

    let (status, started) = send(
        &cell,
        Method::POST,
        &runs("notes"),
        &alice(),
        Some(json!({ "dry_run": true })),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{started}");
    assert_eq!(started["dry_run"], true);
    let run = started["run_id"].as_str().unwrap();
    let report = settled(&cell, &alice(), &format!("{}/{run}", runs("notes"))).await;
    assert_eq!(report["state"], "completed", "{report}");
    assert_eq!(report["dry_run"], true);
    assert_eq!(report["counts"]["admitted"], 150);
    for table in ["memory", "ingest_item", "ingest_cursor", "ingest_run"] {
        assert_eq!(cell.rows(table).await, 0, "{table}");
    }
    cell.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelling_over_http_stops_the_run_within_a_page() {
    // FR-006 through the surface, and the one-run-at-a-time rule.
    let stub = Stub::new("notes", SourceKind::Authenticated, items(500));
    stub.set_faults(Faults {
        park_at: Some(1),
        ..Faults::default()
    });
    let cell = cell_with(&stub).await;

    let (_, started) = send(&cell, Method::POST, &runs("notes"), &alice(), None).await;
    let run = started["run_id"].as_str().unwrap().to_owned();
    stub.reached.notified().await;

    let (status, body) = send(&cell, Method::POST, &runs("notes"), &alice(), None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");

    let path = format!("{}/{run}", runs("notes"));
    let (status, body) = send(&cell, Method::DELETE, &path, &alice(), None).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(body["outcome"], "requested");
    stub.release.notify_one();

    let report = settled(&cell, &alice(), &path).await;
    assert_eq!(report["state"], "cancelled", "{report}");
    assert_eq!(report["cancel_requested"], true);
    assert_eq!(report["counts"]["seen"], 200);
    assert_eq!(cell.rows("memory").await, 200);
    cell.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_routes_refuse_what_they_should_before_a_run_exists() {
    let stub = Stub::new("notes", SourceKind::Authenticated, items(10));
    let cell = cell_with(&stub).await;
    let reader = Who::person("sub-alice", &["memory.read"]);
    let bob = Who::person("sub-bob", &["memory.read", "memory.write"]);

    // The scope gate: starting and cancelling write, so a reader is refused.
    let (status, _) = send(&cell, Method::POST, &runs("notes"), &reader, None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = send(&cell, Method::POST, &runs("notes"), &alice(), None).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let unauthenticated = cell
        .request(Method::POST, &runs("notes"), None)
        .send()
        .await
        .unwrap();
    assert!(
        matches!(
            unauthenticated.status(),
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
        ),
        "{}",
        unauthenticated.status()
    );

    // A source nobody registered, and a name that is not one, are the same 404.
    for path in [runs("nowhere"), runs("Not%20A%20Name")] {
        let (status, _) = send(&cell, Method::POST, &path, &alice(), None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
    }

    // A run is read inside its own scope: another subject, the same ids.
    let (_, started) = send(&cell, Method::POST, &runs("notes"), &bob, None).await;
    let run = started["run_id"].as_str().unwrap().to_owned();
    let path = format!("{}/{run}", runs("notes"));
    let (status, _) = send(&cell, Method::GET, &path, &alice(), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "alice cannot read bob's run");
    let (status, _) = send(&cell, Method::DELETE, &path, &alice(), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "nor cancel it");
    let (status, _) = send(
        &cell,
        Method::GET,
        &format!("{path}?owner=sub-bob"),
        &alice(),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "nor name his scope");
    let (status, _) = send(
        &cell,
        Method::GET,
        &format!("{}/not-a-run", runs("notes")),
        &alice(),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    settled(&cell, &bob, &path).await;

    // Malformed, unknown, and oversized requests start nothing.
    for body in [
        json!({ "surprise": true }),
        json!({ "max_items": 0 }),
        json!({ "rate_per_second": 0 }),
        json!({ "max_items": 99_999_999_999_u64 }),
        json!({ "options": { "blob": "x".repeat(20_000) } }),
        json!({ "scope": { "kind": "elsewhere" } }),
    ] {
        let (status, answer) = send(
            &cell,
            Method::POST,
            &runs("notes"),
            &alice(),
            Some(body.clone()),
        )
        .await;
        assert!(
            status == StatusCode::UNPROCESSABLE_ENTITY || status == StatusCode::BAD_REQUEST,
            "{status} for {body}: {answer}"
        );
    }
    let huge = "x".repeat(40_000);
    let (status, _) = send(
        &cell,
        Method::POST,
        &runs("notes"),
        &alice(),
        Some(json!({ "options": { "huge": huge } })),
    )
    .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    cell.shutdown().await;
}
