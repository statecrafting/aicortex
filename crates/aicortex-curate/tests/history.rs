//! A memory's audit history over a socket, and every Decision-emitting path
//! against the closed reason-code set (spec 024 AC-2, FR-002, FR-005).
//!
//! Tokens are bearer tokens signed by the cell's test key, as 020's and 023's
//! tests use. A browser or OAuth client end to end is the live rauthy harness
//! of 020 D-14, which the history route has no case of its own in.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

// The API's own harness, included by path so there is one (as 023's tests do).
#[path = "../../aicortex-api/tests/common/mod.rs"]
mod common;

use aicortex_curate::Promoter;
use aicortex_store::{Counters, ScopeId, validate_decision};
use aicortex_types::{MemoryKind, Scope, Status};
use common::{Who, capture_body};
use rahi_store::{Statement, TxnBuilder, Value};
use rahi_types::{Sub, UnixSeconds};
use reqwest::{Method, StatusCode, header};
use serde_json::{Value as Json, json};

// A token-shaped value the gate must refuse; assembled from parts so no
// literal credential sits in the source for the secret scan to find.
const SECRET: &str = concat!("ghp_", "9wQk2LmXr4Tb8Zc1Nd6Vf3Hs5Jg7Pa0Ye2Uu");

fn alice() -> Who {
    Who::person(
        "sub-alice",
        &[
            "memory.read",
            "memory.write",
            "memory.promote",
            "memory.delete",
        ],
    )
}

async fn send(
    cell: &common::Cell,
    method: Method,
    path: &str,
    who: &Who,
    body: Option<Json>,
) -> (StatusCode, String) {
    let mut request = cell.request(method, path, Some(who));
    if let Some(body) = body {
        request = request
            .header(header::CONTENT_TYPE, "application/json")
            .body(body.to_string());
    }
    let response = request.send().await.expect("answered");
    let status = response.status();
    (status, response.text().await.expect("a body"))
}

fn parse(text: &str) -> Json {
    serde_json::from_str(text).unwrap_or_else(|error| panic!("{error}: {text}"))
}

async fn ok(
    cell: &common::Cell,
    method: Method,
    path: &str,
    who: &Who,
    body: Option<Json>,
) -> Json {
    let (status, text) = send(cell, method, path, who, body).await;
    assert_eq!(status, StatusCode::OK, "{path}: {text}");
    parse(&text)
}

async fn capture(cell: &common::Cell, who: &Who, text: &str) -> (String, bool) {
    capture_as(cell, who, "observation", text).await
}

async fn capture_as(cell: &common::Cell, who: &Who, kind: &str, text: &str) -> (String, bool) {
    let body = ok(
        cell,
        Method::POST,
        "/api/v1/capture",
        who,
        Some(json!({ "candidates": [{ "kind": kind, "text": text }] })),
    )
    .await;
    let result = &body["results"][0];
    (
        result["id"].as_str().expect("an admitted id").to_owned(),
        result["merged"].as_bool().unwrap_or(false),
    )
}

/// The event labels of a history, in order.
fn events(history: &Json) -> Vec<String> {
    history["events"]
        .as_array()
        .expect("events")
        .iter()
        .map(|event| event["event"].as_str().expect("an event").to_owned())
        .collect()
}

/// Move a captured memory into quarantine, row, record and counters together,
/// as the gate's quarantine verdict leaves it.
async fn quarantine(cell: &common::Cell, id: &str) {
    #[derive(serde::Deserialize)]
    struct Row {
        record: String,
    }
    let store = cell.store.handle();
    let rows: Vec<Row> = store
        .query_consistent(
            "SELECT record FROM memory WHERE id = ?1",
            vec![Value::from(id)],
        )
        .await
        .expect("the row reads");
    let row = rows.into_iter().next().expect("the memory exists");
    let mut record: Json = serde_json::from_str(&row.record).unwrap();
    record["status"] = json!({ "state": "quarantined" });
    let scope_id = ScopeId::of(&Scope::personal(Sub::new("sub-alice")));
    let mut txn = TxnBuilder::new();
    txn.push(Statement::with_params(
        "UPDATE memory SET status = 'quarantined', record = ?2 WHERE id = ?1",
        vec![Value::from(id), Value::from(record.to_string())],
    ));
    Counters::adjust(
        &mut txn,
        &scope_id,
        MemoryKind::Observation,
        &Status::Active,
        -1,
    );
    Counters::adjust(
        &mut txn,
        &scope_id,
        MemoryKind::Observation,
        &Status::Quarantined,
        1,
    );
    store.txn(txn.into_statements()).await.expect("quarantined");
}

/// The open review item of `subject`, listed through the route.
async fn item_of(cell: &common::Cell, who: &Who, subject: &str) -> String {
    let listing = ok(cell, Method::GET, "/api/v1/review", who, None).await;
    listing["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|item| item["subject"] == subject)
        .unwrap_or_else(|| panic!("no open item for {subject}: {listing}"))["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

async fn history(cell: &common::Cell, who: &Who, id: &str) -> Json {
    ok(
        cell,
        Method::GET,
        &format!("/api/v1/memories/{id}/history"),
        who,
        None,
    )
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ac2_the_history_route_returns_the_full_timeline_of_a_memory() {
    let cell = common::boot_routed(aicortex_curate::routes::routes()).await;
    let who = alice();

    // The memory that is later corrected.
    let (wrong, _) = capture(&cell, &who, "the release shipped on tuesday").await;
    let patched = ok(
        &cell,
        Method::PATCH,
        &format!("/api/v1/memories/{wrong}"),
        &who,
        Some(json!({ "correction": { "text": "the release shipped on wednesday" } })),
    )
    .await;
    let correction = patched["superseded_by"].as_str().unwrap().to_owned();

    // The correction is captured again (merged), promoted, then erased.
    let (again, merged) = capture_as(
        &cell,
        &who,
        "correction",
        "the release shipped on wednesday",
    )
    .await;
    assert_eq!((again.as_str(), merged), (correction.as_str(), true));
    let promoted = ok(
        &cell,
        Method::POST,
        &format!("/api/v1/promote/{correction}"),
        &who,
        None,
    )
    .await;
    let promote_decision = promoted["decision_id"].as_str().unwrap().to_owned();

    let before = history(&cell, &who, &correction).await;
    assert_eq!(
        events(&before),
        ["created", "corrects", "merged", "promote"],
        "{before}"
    );
    assert_eq!(before["events"][1]["related"], wrong.as_str());
    assert_eq!(before["events"][3]["decision"], promote_decision.as_str());

    let erased = ok(
        &cell,
        Method::DELETE,
        &format!("/api/v1/memories/{correction}"),
        &who,
        None,
    )
    .await;
    let after = history(&cell, &who, &correction).await;
    // Erasure removes the source log and the derivation rows (014 B-7), so the
    // merge and the correction's target go with the content.
    assert_eq!(
        events(&after),
        ["created", "promote", "erase", "erased"],
        "{after}"
    );
    assert_eq!(
        after["events"][2]["decision"], erased["decision"],
        "{after}"
    );
    assert_eq!(after["truncated"], false);
    assert_eq!(after["statements"], 1);

    // The corrected memory says it was corrected, and by what.
    let old = history(&cell, &who, &wrong).await;
    assert_eq!(events(&old), ["created", "corrected"], "{old}");
    assert_eq!(old["events"][1]["related"], correction.as_str());

    // No content anywhere in a history.
    assert!(!after.to_string().contains("shipped"));

    // Another subject's token cannot read it, and the same id is a 404.
    let bob = Who::person("sub-bob", &["memory.read"]);
    let (status, _) = send(
        &cell,
        Method::GET,
        &format!("/api/v1/memories/{correction}/history"),
        &bob,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = send(
        &cell,
        Method::GET,
        &format!("/api/v1/memories/{correction}/history"),
        &Who::person("sub-alice", &["memory.write"]),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "memory.read is required");
    let (status, _) = send(
        &cell,
        Method::GET,
        "/api/v1/memories/not-an-id/history",
        &who,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    cell.ledger.verify().await.expect("the chain verifies");
    cell.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr002_fr005_every_emitting_path_validates_and_the_chain_verifies() {
    let cell = common::boot_routed(aicortex_curate::routes::routes()).await;
    let who = alice();
    let scope = Scope::personal(Sub::new("sub-alice"));

    // The gate: a refusal.
    let (status, _) = send(
        &cell,
        Method::POST,
        "/api/v1/capture",
        &who,
        Some(capture_body(&format!("my token is {SECRET}"))),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // The review queue: admit, reject, decline.
    let (admitted, _) = capture(&cell, &who, "an origin nobody could establish").await;
    quarantine(&cell, &admitted).await;
    let item = item_of(&cell, &who, &admitted).await;
    ok(
        &cell,
        Method::POST,
        &format!("/api/v1/review/{item}/resolve"),
        &who,
        Some(json!({ "resolution": "admit" })),
    )
    .await;
    let after_admit = history(&cell, &who, &admitted).await;
    assert_eq!(events(&after_admit), ["created", "review_admit"]);

    let (rejected, _) = capture(&cell, &who, "a second doubtful origin").await;
    quarantine(&cell, &rejected).await;
    let item = item_of(&cell, &who, &rejected).await;
    ok(
        &cell,
        Method::POST,
        &format!("/api/v1/review/{item}/resolve"),
        &who,
        Some(json!({ "resolution": "reject" })),
    )
    .await;
    let after_reject = history(&cell, &who, &rejected).await;
    assert_eq!(events(&after_reject), ["created", "erase", "erased"]);

    let (asked, _) = capture(&cell, &who, "please make this a rule").await;
    let (status, text) = send(
        &cell,
        Method::POST,
        "/api/v1/review",
        &who,
        Some(json!({ "kind": "promotion_request", "memory": asked })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{text}");
    let item = item_of(&cell, &who, &asked).await;
    ok(
        &cell,
        Method::POST,
        &format!("/api/v1/review/{item}/resolve"),
        &who,
        Some(json!({ "resolution": "decline" })),
    )
    .await;
    assert_eq!(
        events(&history(&cell, &who, &asked).await),
        ["created", "review_decline"]
    );

    // Promotion: promote then demote, and a promotion that lapses.
    let (rule, _) = capture(&cell, &who, "always run the linter").await;
    ok(
        &cell,
        Method::POST,
        &format!("/api/v1/promote/{rule}"),
        &who,
        None,
    )
    .await;
    ok(
        &cell,
        Method::POST,
        &format!("/api/v1/demote/{rule}"),
        &who,
        None,
    )
    .await;
    assert_eq!(
        events(&history(&cell, &who, &rule).await),
        ["created", "promote", "demote"]
    );

    let (lapsing, _) = capture(&cell, &who, "a rule with an expiry").await;
    ok(
        &cell,
        Method::POST,
        &format!("/api/v1/promote/{lapsing}"),
        &who,
        Some(json!({ "review_interval_secs": 3600 })),
    )
    .await;
    let later = UnixSeconds::new(u64::try_from(chrono_now()).unwrap() + 7200);
    let lapsed = Promoter::new(&cell.store.handle(), &cell.ledger)
        .expire_due(&scope, later)
        .await
        .expect("the sweep runs");
    assert_eq!(lapsed.len(), 1);
    assert_eq!(
        events(&history(&cell, &who, &lapsing).await),
        ["created", "promote", "lapse"]
    );

    // Erasure, through the route.
    let (gone, _) = capture(&cell, &who, "something to forget").await;
    ok(
        &cell,
        Method::DELETE,
        &format!("/api/v1/memories/{gone}"),
        &who,
        None,
    )
    .await;

    // FR-002: every Decision this product wrote validates against the closed
    // set, and the kinds we meant to exercise were all written.
    let mut kinds = std::collections::BTreeSet::new();
    for record in cell.records().await {
        let decision = record.decision().unwrap();
        let kind = decision.kind.as_str().to_owned();
        if kind.starts_with("memory.") || kind.starts_with("review.") {
            validate_decision(&decision).unwrap_or_else(|error| panic!("{kind}: {error}"));
            kinds.insert(kind);
        }
    }
    for expected in [
        "memory.gate.refuse",
        "memory.erase",
        "memory.promote",
        "memory.demote",
        "memory.promotion_lapsed",
        "review.admit",
        "review.decline",
    ] {
        assert!(
            kinds.contains(expected),
            "{expected} missing from {kinds:?}"
        );
    }

    // FR-005: the chain the suite grew still verifies.
    cell.ledger.verify().await.expect("the chain verifies");
    cell.shutdown().await;
}

/// The wall clock in seconds, which is what the API stamps its acts with.
fn chrono_now() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    )
    .unwrap()
}
