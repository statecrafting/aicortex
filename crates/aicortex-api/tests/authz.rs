//! Authorization: the credential, the scope, the interactive principal, and
//! scope reachability (spec 020 FR-001 to FR-004, FR-008, B-2, B-3).
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
const PROMOTE: &str = "memory.promote";

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
    for (method, path) in [
        (Method::GET, "/api/v1/memories"),
        (
            Method::GET,
            "/api/v1/memories/0195f0e0-9b1e-7c1f-9c0e-2c4a4f2b0a11",
        ),
        (Method::GET, "/api/v1/stats"),
        (Method::GET, "/api/v1/events"),
    ] {
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
    // check first (D-10): refused, never admitted, and nothing stored.
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
    // The kernel ledgers the refusal asynchronously; allow a loaded CI host
    // up to 30 seconds, returning as soon as the record lands.
    let mut refused = false;
    for _ in 0..300 {
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
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert!(refused, "the scope refusal is in the chain");

    // The same token reads.
    let response = cell
        .request(Method::GET, "/api/v1/memories", Some(&reader))
        .send()
        .await
        .expect("answered");
    assert_eq!(response.status(), StatusCode::OK);
    cell.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr003_a_client_credentials_token_never_reaches_promote() {
    let cell = common::boot().await;
    let person = Who::person("sub-promoter", &[READ, WRITE, PROMOTE]);
    let id = captured(&cell, &person, "a memory someone may promote").await;

    // A client acting as itself, holding every scope there is.
    let every: Vec<&'static str> = ApiScope::ALL.iter().map(|scope| scope.as_str()).collect();
    let service = Who::service("importer", &every);
    let response = cell
        .request(
            Method::POST,
            &format!("/api/v1/promote/{id}"),
            Some(&service),
        )
        .send()
        .await
        .expect("answered");
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(cell.rows("memory").await, 1);

    // The same service reaches the service-callable rest of the surface.
    let response = cell
        .request(Method::GET, "/api/v1/memories", Some(&service))
        .send()
        .await
        .expect("answered");
    assert_eq!(response.status(), StatusCode::OK);

    // A person with the scope reaches the route, which waits on 023 (D-12).
    let response = cell
        .request(
            Method::POST,
            &format!("/api/v1/promote/{id}"),
            Some(&person),
        )
        .send()
        .await
        .expect("answered");
    assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
    let body = json(response).await;
    assert_eq!(body["code"], "promotion_unavailable");

    // Without the scope, the gate refuses first.
    let writer = Who::person("sub-promoter", &[READ, WRITE]);
    let response = cell
        .request(
            Method::POST,
            &format!("/api/v1/promote/{id}"),
            Some(&writer),
        )
        .send()
        .await
        .expect("answered");
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    cell.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr004_an_unreachable_scope_is_a_404_that_says_nothing() {
    let cell = common::boot().await;
    let alice = Who::person("sub-alice", &[READ, WRITE]);
    let bob = Who::person("sub-bob", &[READ, WRITE]);
    let id = captured(&cell, &alice, "alice's own note").await;
    let absent = aicortex_types::MemoryId::now_v7().to_string();

    // Alice reads her own memory.
    let response = cell
        .request(Method::GET, &format!("/api/v1/memories/{id}"), Some(&alice))
        .send()
        .await
        .expect("answered");
    assert_eq!(response.status(), StatusCode::OK);

    // Bob naming Alice's scope: a held id and an absent one answer the same,
    // byte for byte, and so does a listing or a count of that scope.
    let mut answers = BTreeSet::new();
    for path in [
        format!("/api/v1/memories/{id}?owner=sub-alice"),
        format!("/api/v1/memories/{absent}?owner=sub-alice"),
        format!("/api/v1/memories/{id}?owner=sub-carol"),
        "/api/v1/memories?owner=sub-alice".to_owned(),
        "/api/v1/stats?owner=sub-alice".to_owned(),
        format!("/api/v1/memories/{id}?scope=project&project=p&owner=sub-alice"),
    ] {
        let response = cell
            .request(Method::GET, &path, Some(&bob))
            .send()
            .await
            .expect("answered");
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        answers.insert(response.bytes().await.expect("a body"));
    }
    // Bob naming Alice's memory in his own scope finds nothing either.
    let response = cell
        .request(Method::GET, &format!("/api/v1/memories/{id}"), Some(&bob))
        .send()
        .await
        .expect("answered");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    answers.insert(response.bytes().await.expect("a body"));
    assert_eq!(answers.len(), 1, "every 404 is the same document");

    // Nor can Bob write into, erase from, or patch in Alice's scope.
    let response = cell
        .request(
            Method::DELETE,
            &format!("/api/v1/memories/{id}?owner=sub-alice"),
            Some(&Who::person("sub-bob", &["memory.delete"])),
        )
        .send()
        .await
        .expect("answered");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let response = cell
        .request(Method::POST, "/api/v1/capture", Some(&bob))
        .header(header::CONTENT_TYPE, "application/json")
        .body(
            serde_json::json!({
                "scope": { "kind": "personal", "owner": "sub-alice" },
                "candidates": [{ "kind": "fact", "text": "planted" }]
            })
            .to_string(),
        )
        .send()
        .await
        .expect("answered");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
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
