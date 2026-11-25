//! AC-2: the API against a real rauthy (spec 020 D-14).
//!
//! A client is registered through dynamic registration, a person completes
//! authorization code with PKCE, and the access token rauthy signs (RS256,
//! its own key, its own issuer, the cell's origin in `aud`) is what the
//! chassis's bearer layer validates in front of the real routes. Nothing is
//! signed or edited by the test. The final step reads the captured memory
//! back with `GET /memories/{id}` until 018 mounts `POST /search` (D-3).
//!
//! Needs `AICORTEX_TEST_RAUTHY_URL` and its companions (`common/live.rs`),
//! which `scripts/live-rauthy.sh up` prints; skipped without them unless
//! `AICORTEX_REQUIRE_RAUTHY=1`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

mod common;

use aicortex_recall::FRAMING_STATEMENT;
use common::capture_body;
use common::live::{LiveEnv, claims, person_token};
use reqwest::{Method, StatusCode, header};
use serde_json::{Value, json};

const TEXT: &str = "the live rauthy admitted this memory";

async fn call(
    cell: &common::Cell,
    method: Method,
    path: &str,
    token: &str,
    body: Option<Value>,
) -> (StatusCode, header::HeaderMap, String) {
    let mut request = cell.http.request(method, cell.url(path)).bearer_auth(token);
    if let Some(body) = body {
        request = request
            .header(header::CONTENT_TYPE, "application/json")
            .body(body.to_string());
    }
    let response = request.send().await.expect("answered");
    let status = response.status();
    let headers = response.headers().clone();
    (status, headers, response.text().await.expect("a body"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ac2_a_real_rauthy_token_captures_and_reads_back() {
    let Some(live) = LiveEnv::from_env("ac2") else {
        return;
    };
    let cell = common::boot_live(&live).await;

    // The audience is the origin rauthy publishes: it is the cell's public
    // URL here, as it is when the cell serves rauthy same-origin.
    let token = person_token(&live, &live.origin, &["memory.read", "memory.write"]).await;
    let claims = claims(&token);
    assert_eq!(claims["iss"], json!(format!("{}/auth/v1/", live.origin)));
    assert!(
        claims["aud"]
            .as_array()
            .unwrap()
            .contains(&json!(live.origin))
    );
    assert_eq!(claims["scope"], json!("openid memory.read memory.write"));
    assert!(claims["sub"].as_str().is_some_and(|sub| !sub.is_empty()));

    // No credential: the challenge that names the resource metadata (FR-001).
    let anonymous = cell
        .http
        .get(cell.url("/api/v1/memories"))
        .send()
        .await
        .expect("answered");
    assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED);

    // Capture with the token, then read the memory back.
    let (status, _, text) = call(
        &cell,
        Method::POST,
        "/api/v1/capture",
        &token,
        Some(capture_body(TEXT)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let id = serde_json::from_str::<Value>(&text).unwrap()["results"][0]["id"]
        .as_str()
        .expect("an admitted id")
        .to_owned();

    let (status, _, text) = call(
        &cell,
        Method::GET,
        &format!("/api/v1/memories/{id}"),
        &token,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert!(text.contains(TEXT), "the memory reads back: {text}");
    assert!(
        text.contains(FRAMING_STATEMENT),
        "inside the envelope: {text}"
    );

    // The same token is refused a scope it was never granted (FR-002's shape).
    let (status, headers, text) = call(
        &cell,
        Method::DELETE,
        &format!("/api/v1/memories/{id}"),
        &token,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{text}");
    let challenge = headers[header::WWW_AUTHENTICATE].to_str().unwrap();
    assert!(
        challenge.contains("insufficient_scope") && challenge.contains("memory.delete"),
        "{challenge}"
    );

    // A token for another resource is refused on its audience.
    let other = person_token(&live, "https://other-resource.invalid", &["memory.read"]).await;
    let (status, headers, text) = call(&cell, Method::GET, "/api/v1/memories", &other, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{text}");
    assert!(
        headers[header::WWW_AUTHENTICATE]
            .to_str()
            .unwrap()
            .contains("invalid_token")
    );
    cell.shutdown().await;
}
