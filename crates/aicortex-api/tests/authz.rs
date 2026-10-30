//! Authorization: the credential, the scope, and scope reachability (spec
//! 020 FR-001, FR-002, FR-004, FR-008, B-2, B-3).
//!
//! Every request goes over a socket into the chassis's bearer layer and edge;
//! every refusal asserted here is the one a client receives.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

mod common;

use std::collections::BTreeSet;

use aicortex_api::ApiScope;
use common::{Who, capture_body};
use reqwest::{Method, StatusCode, header};
use serde_json::Value;

const READ: &str = "memory.read";
const WRITE: &str = "memory.write";

async fn json(response: reqwest::Response) -> Value {
    let bytes = response.bytes().await.expect("a body");
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

/// Capture one memory as `who` and return its id.
async fn captured(cell: &common::Cell, who: &Who, text: &str) -> String {
    let response = cell
        .request(Method::POST, "/api/v1/capture", Some(who))
        .header(header::CONTENT_TYPE, "application/json")
        .body(capture_body(text).to_string())
        .send()
        .await
        .expect("the capture is answered");
    assert_eq!(response.status(), StatusCode::OK);
    let body = json(response).await;
    body["results"][0]["id"]
        .as_str()
        .expect("an admitted id")
        .to_owned()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr001_no_credential_is_a_401_naming_the_resource_metadata() {
    let cell = common::boot().await;
    for (method, path) in [(Method::GET, "/api/v1/events")] {
        let response = cell
            .request(method.clone(), path, None)
            .send()
            .await
            .expect("answered");
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "{method} {path}"
        );
        let challenge = response
            .headers()
            .get(header::WWW_AUTHENTICATE)
            .expect("a challenge")
            .to_str()
            .expect("ascii")
            .to_owned();
        let metadata = format!(
            "resource_metadata=\"{}/.well-known/oauth-protected-resource\"",
            cell.origin
        );
        assert!(challenge.starts_with("Bearer "), "{challenge}");
        assert!(challenge.contains(&metadata), "{challenge}");
    }
    // An unsafe method with no credential at all meets the chassis's CSRF
    // check first (D-12): refused, never admitted, and nothing stored.
    let response = cell
        .request(Method::POST, "/api/v1/capture", None)
        .body(capture_body("anonymous").to_string())
        .send()
        .await
        .expect("answered");
    assert!(response.status().is_client_error(), "{}", response.status());
    assert_eq!(cell.rows("memory").await, 0);
    cell.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr002_a_read_token_is_refused_capture_naming_memory_write() {
    let cell = common::boot().await;
    let reader = Who::person("sub-reader", &[READ]);
    let before = cell.records().await.len();
    let response = cell
        .request(Method::POST, "/api/v1/capture", Some(&reader))
        .header(header::CONTENT_TYPE, "application/json")
        .body(capture_body("a note the reader may not write").to_string())
        .send()
        .await
        .expect("answered");
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let challenge = response
        .headers()
        .get(header::WWW_AUTHENTICATE)
        .expect("a challenge")
        .to_str()
        .expect("ascii")
        .to_owned();
    assert_eq!(
        challenge,
        "Bearer error=\"insufficient_scope\", scope=\"memory.write\""
    );
    assert_eq!(cell.rows("memory").await, 0, "nothing was stored");
    // The kernel ledgers the refusal; give its writer a moment.
    let mut refused = false;
    for _ in 0..50 {
        refused = cell
            .records()
            .await
            .iter()
            .skip(before)
            .filter_map(|record| record.decision().ok())
            .any(|decision| decision.kind.as_str() == rahi_idp::scope::DECISION_SCOPE_DENIED);
        if refused {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(refused, "the scope refusal is in the chain");

    cell.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr004_an_unreachable_scope_is_a_404_that_says_nothing() {
    let cell = common::boot().await;
    let alice = Who::person("sub-alice", &[READ, WRITE]);
    let bob = Who::person("sub-bob", &[READ, WRITE]);
    captured(&cell, &alice, "alice's own note").await;

    // Bob naming a scope Alice owns, whether it holds rows or not, gets the
    // same document, and nothing is written.
    let mut answers = BTreeSet::new();
    for scope in [
        serde_json::json!({ "kind": "personal", "owner": "sub-alice" }),
        serde_json::json!({ "kind": "project", "project": "p", "owner": "sub-alice" }),
        serde_json::json!({ "kind": "personal", "owner": "sub-carol" }),
    ] {
        let response = cell
            .request(Method::POST, "/api/v1/capture", Some(&bob))
            .header(header::CONTENT_TYPE, "application/json")
            .body(
                serde_json::json!({
                    "scope": scope,
                    "candidates": [{ "kind": "fact", "text": "planted" }]
                })
                .to_string(),
            )
            .send()
            .await
            .expect("answered");
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{scope}");
        answers.insert(response.bytes().await.expect("a body"));
    }
    assert_eq!(answers.len(), 1, "every 404 is the same document");
    assert_eq!(cell.rows("memory").await, 1);
    cell.shutdown().await;
}

#[test]
fn fr008_seven_scopes_and_neither_family_satisfies_the_other() {
    let names: Vec<&str> = ApiScope::ALL.iter().map(|scope| scope.as_str()).collect();
    assert_eq!(
        names,
        [
            "memory.read",
            "memory.write",
            "memory.delete",
            "memory.admin",
            "memory.promote",
            "coordination.read",
            "coordination.write",
        ]
    );
    for required in ApiScope::ALL {
        for held in ApiScope::ALL {
            let granted = BTreeSet::from([held.as_str().to_owned()]);
            let memory = |scope: ApiScope| scope.as_str().starts_with("memory.");
            if memory(required) != memory(held) {
                assert!(
                    !required.satisfied_by(&granted),
                    "{} satisfied {}",
                    held.as_str(),
                    required.as_str()
                );
            }
            assert_eq!(required.satisfied_by(&granted), required == held);
        }
        // Every memory scope at once still satisfies no coordination scope,
        // and the reverse.
        let family: BTreeSet<String> = ApiScope::ALL
            .iter()
            .filter(|scope| {
                scope.as_str().starts_with("memory.") != required.as_str().starts_with("memory.")
            })
            .map(|scope| scope.as_str().to_owned())
            .collect();
        assert!(!required.satisfied_by(&family));
    }
}
