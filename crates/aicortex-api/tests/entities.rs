//! `GET /entities` and `GET /entities/{id}/neighbors` through the chassis
//! (spec 020 B-4, D-3; spec 017 D-9).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

mod common;

use aicortex_graph::{ExtractionConfig, ExtractionWorker, PatternExtractor};
use aicortex_recall::FRAMING_STATEMENT;
use aicortex_store::{ScopeId, stage_extraction};
use aicortex_types::{MemoryId, Scope};
use common::{Who, capture_body};
use rahi_store::TxnBuilder;
use rahi_types::{Sub, UnixSeconds};
use reqwest::{Method, StatusCode};
use serde_json::Value;

const READ: &[&str] = &["memory.read", "memory.write"];

async fn get(cell: &common::Cell, path: &str, who: &Who) -> (StatusCode, Value) {
    let response = cell
        .request(Method::GET, path, Some(who))
        .send()
        .await
        .expect("answered");
    let status = response.status();
    let text = response.text().await.expect("a body");
    (
        status,
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("{e}: {text}")),
    )
}

async fn capture_and_extract(cell: &common::Cell, who: &Who, owner: &str, text: &str) -> String {
    let response = cell
        .request(Method::POST, "/api/v1/capture", Some(who))
        .header("content-type", "application/json")
        .body(capture_body(text).to_string())
        .send()
        .await
        .expect("answered");
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = response.json().await.expect("json");
    let id = body["results"][0]["id"].as_str().unwrap().to_owned();
    let scope_id = ScopeId::of(&Scope::personal(Sub::new(owner)));
    let store = cell.store.handle();
    let mut txn = TxnBuilder::new();
    stage_extraction(
        &mut txn,
        scope_id.as_str(),
        id.parse::<MemoryId>().unwrap(),
        PatternExtractor::NAME,
        PatternExtractor::VERSION,
        UnixSeconds::new(1_700_000_001),
    )
    .expect("the work stages");
    store.txn(txn.into_statements()).await.expect("commits");
    let report = ExtractionWorker::new(
        PatternExtractor,
        ExtractionConfig::standard(),
        "api-test-worker",
    )
    .expect("a worker")
    .drain(&store, UnixSeconds::new(1_700_000_100))
    .await
    .expect("a drain");
    assert_eq!(report.completed, 1);
    id
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn entities_list_and_walk_are_scoped_framed_and_bodyless() {
    let cell = common::boot().await;
    let alice = Who::person("sub-alice", READ);
    let bob = Who::person("sub-bob", READ);
    let memory = capture_and_extract(
        &cell,
        &alice,
        "sub-alice",
        "Dana Whitfield works on Apollo. Apollo depends on Hermes.",
    )
    .await;

    let (status, body) = get(&cell, "/api/v1/entities", &alice).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["framing"], FRAMING_STATEMENT);
    assert_eq!(body["derived"], true);
    let items = body["items"].as_array().unwrap();
    let names: Vec<&str> = items
        .iter()
        .map(|i| i["canonical"].as_str().unwrap())
        .collect();
    assert_eq!(items.len(), 3, "{names:?}");
    for want in ["Dana Whitfield", "Apollo", "Hermes"] {
        assert!(names.contains(&want), "{want} in {names:?}");
    }

    let (_, body) = get(&cell, "/api/v1/entities?kind=person", &alice).await;
    assert_eq!(body["items"].as_array().unwrap().len(), 1);
    let (status, _) = get(&cell, "/api/v1/entities?kind=wizard", &alice).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // A page of one names the next, and the next page continues after it.
    let (_, first) = get(&cell, "/api/v1/entities?limit=1", &alice).await;
    let cursor = first["next"].as_str().unwrap().to_owned();
    let (_, second) = get(
        &cell,
        &format!("/api/v1/entities?limit=1&cursor={cursor}"),
        &alice,
    )
    .await;
    assert_ne!(first["items"][0]["id"], second["items"][0]["id"]);

    // The walk from Apollo reaches the memory by a path and carries no body.
    let apollo = items.iter().find(|i| i["canonical"] == "Apollo").unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let path = format!("/api/v1/entities/{apollo}/neighbors");
    let (status, walk) = get(&cell, &path, &alice).await;
    assert_eq!(status, StatusCode::OK, "{walk}");
    assert_eq!(walk["framing"], FRAMING_STATEMENT);
    let hits = walk["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0]["memory"], memory.as_str());
    assert!(!hits[0]["path"].as_array().unwrap().is_empty());
    assert!(!walk.to_string().contains("works on Apollo"));
    let (status, _) = get(&cell, &format!("{path}?predicates=nope"), &alice).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (_, narrow) = get(&cell, &format!("{path}?predicates=caused"), &alice).await;
    assert!(narrow["hits"].as_array().unwrap().is_empty());

    // Another subject sees none of it, and one scope answers one way (FR-004).
    let (_, theirs) = get(&cell, "/api/v1/entities", &bob).await;
    assert!(theirs["items"].as_array().unwrap().is_empty());
    let (status, _) = get(&cell, &path, &bob).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = get(&cell, "/api/v1/entities?owner=sub-alice", &bob).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // The scope gate: no memory.read, no entities.
    let write_only = Who::person("sub-alice", &["memory.write"]);
    let (status, _) = get(&cell, "/api/v1/entities", &write_only).await;
    assert!(
        status == StatusCode::FORBIDDEN || status == StatusCode::UNAUTHORIZED,
        "{status}"
    );
    cell.shutdown().await;
}
