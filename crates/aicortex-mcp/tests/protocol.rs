//! The wire: transport, negotiation, errors, and the bootstrap challenge
//! (spec 021 B-1 to B-4, FR-001 to FR-003), replayed from recorded sessions.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

mod support;

use std::time::Duration;

use aicortex_mcp::Revision;
use reqwest::{StatusCode, header};
use serde_json::{Value, json};
use support::{ALL, Who, post, request, rpc};

/// Whether every member of `expected` is in `actual`, arrays element by
/// element and of equal length, with `"*"` matching anything.
fn matches(expected: &Value, actual: &Value) -> bool {
    match (expected, actual) {
        (Value::String(text), _) if text == "*" => true,
        (Value::Object(want), Value::Object(have)) => want
            .iter()
            .all(|(key, want)| have.get(key).is_some_and(|have| matches(want, have))),
        (Value::Array(want), Value::Array(have)) => {
            want.len() == have.len() && want.iter().zip(have).all(|(w, h)| matches(w, h))
        }
        _ => expected == actual,
    }
}

fn fixture(revision: Revision) -> Value {
    let path = format!(
        "{}/testdata/sessions/{}.json",
        env!("CARGO_MANIFEST_DIR"),
        revision.as_str()
    );
    serde_json::from_str(&std::fs::read_to_string(&path).expect("the fixture reads"))
        .expect("the fixture parses")
}

/// FR-001: `initialize`, `tools/list`, and `tools/call` succeed against the
/// recorded session of each supported revision.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr001_each_revision_replays_its_recorded_session() {
    let cell = support::boot().await;
    let token = cell.token(&Who::person("sub-alice", ALL));
    for revision in Revision::ALL {
        let session = fixture(revision);
        assert_eq!(session["revision"], revision.as_str());
        for step in session["steps"].as_array().expect("steps") {
            let name = step["name"].as_str().unwrap();
            let headers: Vec<(String, String)> = step["headers"]
                .as_object()
                .map(|headers| {
                    headers
                        .iter()
                        .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_owned()))
                        .collect()
                })
                .unwrap_or_default();
            let headers: Vec<(&str, &str)> = headers
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect();
            let reply = post(&cell, "/mcp", Some(&token), &headers, &step["request"]).await;
            assert_eq!(
                reply.status.as_u16(),
                u16::try_from(step["status"].as_u64().unwrap()).unwrap(),
                "{revision:?} {name}: {}",
                reply.text
            );
            if reply.status == StatusCode::ACCEPTED {
                assert!(reply.text.is_empty(), "{revision:?} {name}: {}", reply.text);
                continue;
            }
            let message = reply.message();
            assert!(
                matches(&step["expect"], &message),
                "{revision:?} {name}: expected {} of {message}",
                step["expect"]
            );
            for pointer in step["absent"].as_array().into_iter().flatten() {
                assert!(
                    message.pointer(pointer.as_str().unwrap()).is_none(),
                    "{revision:?} {name}: {pointer} is present in {message}"
                );
            }
        }
    }
    cell.shutdown().await;
}

/// FR-002, first half: no credential is a 401 whose challenge names the
/// resource metadata (B-4), on a safe request and, with the edge letting a
/// declared bearer route through, on the POST an MCP client opens with.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr002_no_credential_is_the_discovery_challenge() {
    let cell = support::boot_open().await;
    let initialize = request(1, "initialize", json!({ "protocolVersion": "2025-06-18" }));
    let posted = post(&cell, "/mcp", None, &[], &initialize).await;
    let got = cell.http.get(cell.url("/mcp")).send().await.unwrap();
    for (status, headers) in [
        (posted.status, &posted.headers),
        (got.status(), got.headers()),
    ] {
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let challenge = headers[header::WWW_AUTHENTICATE].to_str().unwrap();
        assert!(
            challenge.starts_with("Bearer") && challenge.contains("resource_metadata=\""),
            "{challenge}"
        );
    }
    // A credential that is not valid is the same status with `invalid_token`.
    let bad = post(&cell, "/mcp", Some("not-a-jwt"), &[], &initialize).await;
    assert_eq!(bad.status, StatusCode::UNAUTHORIZED);
    assert!(
        bad.headers[header::WWW_AUTHENTICATE]
            .to_str()
            .unwrap()
            .contains("invalid_token")
    );
    cell.shutdown().await;
}

/// The chassis at rahi 0.7.1 answers an anonymous POST 403 at its CSRF check
/// before any 401 (020 D-10), so the bootstrap's first request cannot be
/// challenged until the chassis exempts a declared bearer route that carries no
/// credential. This pins what `serve` does today so the day it changes is seen.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_chassis_refuses_an_anonymous_post_before_the_challenge() {
    let cell = support::boot().await;
    let initialize = request(1, "initialize", json!({ "protocolVersion": "2025-06-18" }));
    let posted = post(&cell, "/mcp", None, &[], &initialize).await;
    assert_eq!(posted.status, StatusCode::FORBIDDEN, "{}", posted.text);
    assert!(!posted.headers.contains_key(header::WWW_AUTHENTICATE));
    cell.shutdown().await;
}

/// FR-002, second half, and B-3: an authorization failure inside an
/// established session is a JSON-RPC error at HTTP 200, naming the scope.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr002_an_authorization_failure_in_a_session_is_a_200_json_rpc_error() {
    let cell = support::boot().await;
    let reader = cell.token(&Who::person("sub-alice", &["memory.read"]));
    let reply = post(
        &cell,
        "/mcp",
        Some(&reader),
        &[],
        &request(
            7,
            "tools/call",
            json!({ "name": "capture", "arguments": { "candidates": [{ "kind": "observation", "text": "x" }] } }),
        ),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    let message = reply.message();
    assert_eq!(message["id"], 7);
    assert_eq!(message["error"]["code"], -32001, "{message}");
    assert_eq!(message["error"]["data"]["code"], "insufficient_scope");
    assert_eq!(message["error"]["data"]["scope"], "memory.write");
    assert_eq!(
        cell.rows("memory").await,
        0,
        "the refused call wrote nothing"
    );
    cell.shutdown().await;
}

/// FR-003: `Accept: application/json` alone is answered as a JSON body, and
/// `text/event-stream` alone as a stream; both, or neither header, is JSON.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr003_the_accept_header_picks_the_form() {
    let cell = support::boot().await;
    let token = cell.token(&Who::person("sub-alice", ALL));
    let ping = request(3, "ping", json!({}));
    let kind = |reply: &support::Reply| {
        reply.headers[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .to_owned()
    };

    let json_only = post(
        &cell,
        "/mcp",
        Some(&token),
        &[("accept", "application/json")],
        &ping,
    )
    .await;
    assert_eq!(json_only.status, StatusCode::OK);
    assert!(kind(&json_only).starts_with("application/json"));
    assert_eq!(json_only.message()["result"], json!({}));

    let stream_only = post(
        &cell,
        "/mcp",
        Some(&token),
        &[("accept", "text/event-stream")],
        &ping,
    )
    .await;
    assert_eq!(stream_only.status, StatusCode::OK, "{}", stream_only.text);
    assert!(kind(&stream_only).starts_with("text/event-stream"));
    assert!(
        stream_only.text.contains("event: message"),
        "{}",
        stream_only.text
    );
    assert_eq!(stream_only.message()["id"], 3);

    for accept in [
        Some("application/json, text/event-stream"),
        Some("*/*"),
        None,
    ] {
        let headers: Vec<(&str, &str)> = accept.map(|a| ("accept", a)).into_iter().collect();
        let both = post(&cell, "/mcp", Some(&token), &headers, &ping).await;
        assert!(kind(&both).starts_with("application/json"), "{accept:?}");
    }

    let neither = post(
        &cell,
        "/mcp",
        Some(&token),
        &[("accept", "text/html")],
        &ping,
    )
    .await;
    assert_eq!(neither.status, StatusCode::NOT_ACCEPTABLE);
    cell.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn protocol_errors_are_json_rpc_errors_at_200() {
    let cell = support::boot().await;
    let token = cell.token(&Who::person("sub-alice", ALL));
    let send = |body: String| {
        let cell = &cell;
        let token = token.clone();
        async move {
            let reply = cell
                .http
                .post(cell.url("/mcp"))
                .bearer_auth(token)
                .header(header::CONTENT_TYPE, "application/json")
                .body(body)
                .send()
                .await
                .unwrap();
            let status = reply.status();
            (
                status,
                serde_json::from_str::<Value>(&reply.text().await.unwrap()).unwrap(),
            )
        }
    };
    let (status, message) = send("{not json".to_owned()).await;
    assert_eq!(
        (status, message["error"]["code"].as_i64()),
        (StatusCode::OK, Some(-32700))
    );
    assert_eq!(message["id"], Value::Null);

    let (status, message) = send(json!([request(1, "ping", json!({}))]).to_string()).await;
    assert_eq!(
        (status, message["error"]["code"].as_i64()),
        (StatusCode::OK, Some(-32600))
    );

    let (status, message) = send(request(2, "no/such", json!({})).to_string()).await;
    assert_eq!(
        (status, message["error"]["code"].as_i64()),
        (StatusCode::OK, Some(-32601))
    );
    assert_eq!(message["id"], 2);

    let (status, message) =
        send(json!({ "jsonrpc": "1.0", "id": 4, "method": "ping" }).to_string()).await;
    assert_eq!(
        (status, message["error"]["code"].as_i64()),
        (StatusCode::OK, Some(-32600))
    );

    // An unknown tool and a schema violation are invalid params, and no row
    // is written.
    for params in [
        json!({ "name": "promote", "arguments": {} }),
        json!({ "name": "fetch", "arguments": { "id": "x", "surplus": 1 } }),
        json!({ "arguments": {} }),
    ] {
        let (status, message) = send(request(5, "tools/call", params).to_string()).await;
        assert_eq!(
            (status, message["error"]["code"].as_i64()),
            (StatusCode::OK, Some(-32602))
        );
    }
    cell.shutdown().await;
}

/// B-2 and the revision rules: negotiation answers a revision this server
/// speaks, a header it does not speak is a protocol error, and a session id is
/// echoed and used for nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn negotiation_notifications_and_the_session_header() {
    let cell = support::boot().await;
    let token = cell.token(&Who::person("sub-alice", ALL));

    let unknown = rpc(
        &cell,
        &token,
        Revision::LATEST,
        "initialize",
        json!({ "protocolVersion": "2031-01-01" }),
    )
    .await;
    assert_eq!(
        unknown["result"]["protocolVersion"],
        Revision::LATEST.as_str()
    );
    let old = rpc(
        &cell,
        &token,
        Revision::LATEST,
        "initialize",
        json!({ "protocolVersion": "2025-03-26" }),
    )
    .await;
    assert_eq!(old["result"]["protocolVersion"], "2025-03-26");
    assert_eq!(old["result"]["serverInfo"]["name"], "aicortex");
    assert!(old["result"]["capabilities"]["prompts"].is_null());

    let bad_header = post(
        &cell,
        "/mcp",
        Some(&token),
        &[("mcp-protocol-version", "2031-01-01")],
        &request(1, "ping", json!({})),
    )
    .await;
    assert_eq!(bad_header.status, StatusCode::OK);
    assert_eq!(bad_header.message()["error"]["code"], -32600);

    let notification = post(
        &cell,
        "/mcp",
        Some(&token),
        &[("mcp-session-id", "client-chosen-id")],
        &json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
    )
    .await;
    assert_eq!(notification.status, StatusCode::ACCEPTED);
    assert_eq!(notification.headers["mcp-session-id"], "client-chosen-id");
    assert!(notification.text.is_empty());

    // The id decides nothing: a token with no scope is refused whatever it says.
    let nobody = cell.token(&Who::person("sub-bob", &[]));
    let reply = post(
        &cell,
        "/mcp",
        Some(&nobody),
        &[("mcp-session-id", "an-admin-session")],
        &request(1, "tools/call", json!({ "name": "list", "arguments": {} })),
    )
    .await;
    assert_eq!(
        reply.message()["error"]["data"]["code"],
        "insufficient_scope"
    );
    cell.shutdown().await;
}

/// B-1: `GET` is the server-initiated stream, held open under the chassis's
/// budget, kept alive, and closed by its shutdown event.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn get_is_a_declared_stream_that_closes_on_shutdown() {
    let cell = support::boot().await;
    let token = cell.token(&Who::person("sub-alice", ALL));
    let refused = cell
        .http
        .get(cell.url("/mcp"))
        .bearer_auth(&token)
        .header("accept", "application/json")
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), StatusCode::NOT_ACCEPTABLE);

    let mut stream = cell
        .http
        .get(cell.url("/mcp"))
        .bearer_auth(&token)
        .header("accept", "text/event-stream")
        .send()
        .await
        .unwrap();
    assert_eq!(stream.status(), StatusCode::OK);
    assert!(
        stream.headers()[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .starts_with("text/event-stream")
    );
    tokio::time::sleep(support::common::KEEP_ALIVE * 3).await;
    let reader = tokio::spawn(async move {
        let mut text = String::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while !text.contains("event: shutdown") {
            match tokio::time::timeout_at(deadline, stream.chunk()).await {
                Ok(Ok(Some(chunk))) => text.push_str(&String::from_utf8_lossy(&chunk)),
                _ => break,
            }
        }
        text
    });
    assert_eq!(cell.hub.drain().await, 0);
    let text = reader.await.unwrap();
    assert!(text.contains(": keep-alive"), "{text}");
    assert!(text.contains("event: shutdown"), "{text}");
    assert!(
        !text.contains("\"jsonrpc\""),
        "this server initiates no message: {text}"
    );
    cell.shutdown().await;
}
