//! AC-2: the MCP server against a real rauthy (spec 021, 020 D-14).
//!
//! The harness is spec 020's: `scripts/live-rauthy.sh up` starts the pinned
//! rauthy, `common/live.rs` registers a client, completes authorization code
//! with PKCE as a person, and the access token rauthy signs is what the
//! chassis's bearer layer validates in front of `/mcp`. The cell is composed as
//! `serve` composes it.
//!
//! Two parts. The first speaks the protocol over HTTP with that token:
//! `initialize`, `tools/list`, `capture`, and the read back with `fetch` (018
//! has not landed `search`, 020 D-3). The second runs a real MCP client, the
//! official TypeScript SDK, when `AICORTEX_MCP_SDK` names a directory it is
//! installed in (`npm install @modelcontextprotocol/sdk`), once with the token
//! and once anonymously, which is how a client begins the bootstrap of B-4.
//! The anonymous run is recorded, not hidden: at rahi 0.7.1 it ends at the
//! chassis's CSRF check (020 D-10), so AC-2's bootstrap is not met.
//!
//! Needs `AICORTEX_TEST_RAUTHY_URL` and its companions; skipped without them
//! unless `AICORTEX_REQUIRE_RAUTHY=1`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

mod support;

use std::process::Command;

use aicortex_recall::FRAMING_STATEMENT;
use reqwest::{StatusCode, header};
use serde_json::{Value, json};
use support::common::live::{LiveEnv, claims, person_token};
use support::{call, request, rpc, structured};

const TEXT: &str = "the live rauthy admitted this memory through the mcp server";

async fn run_client(url: String, token: Option<String>, sdk: String) -> (i32, String, String) {
    tokio::task::spawn_blocking(move || {
        let script = format!(
            "{}/../../scripts/mcp-sdk-client.mjs",
            env!("CARGO_MANIFEST_DIR")
        );
        let mut command = Command::new("node");
        command.arg(script).env("MCP_URL", url).env("MCP_SDK", sdk);
        if let Some(token) = token {
            command.env("MCP_TOKEN", token);
        }
        let output = command.output().expect("node runs");
        (
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    })
    .await
    .expect("the client finishes")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ac2_a_real_rauthy_token_captures_and_reads_back_through_mcp() {
    let Some(live) = LiveEnv::from_env("mcp-ac2") else {
        return;
    };
    let cell = support::common::boot_mounting(
        Some(&live),
        Box::new(|api| support::common::Extra {
            router: aicortex_mcp::router(api.clone()),
            bearer: aicortex_mcp::bearer_routes(aicortex_api::bearer_routes()),
            open_bootstrap: false,
        }),
    )
    .await;
    let token = person_token(&live, &live.origin, &["memory.read", "memory.write"]).await;
    assert_eq!(
        claims(&token)["scope"],
        json!("openid memory.read memory.write")
    );

    // The protocol, over HTTP, with rauthy's token.
    let initialized = rpc(
        &cell,
        &token,
        aicortex_mcp::Revision::LATEST,
        "initialize",
        json!({ "protocolVersion": "2025-06-18" }),
    )
    .await;
    assert_eq!(initialized["result"]["serverInfo"]["name"], "aicortex");
    let captured = call(
        &cell,
        &token,
        "capture",
        json!({ "candidates": [{ "kind": "observation", "text": TEXT }] }),
    )
    .await;
    let id = structured(&captured)["results"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let fetched = call(&cell, &token, "fetch", json!({ "id": id })).await;
    let body = structured(&fetched);
    assert_eq!(body["framing"], FRAMING_STATEMENT);
    assert!(body["memories"][0].as_str().unwrap().contains(TEXT));

    // The token is refused what it was never granted, inside the session.
    let forgotten = call(&cell, &token, "forget", json!({ "id": id })).await;
    assert_eq!(forgotten["error"]["data"]["code"], "insufficient_scope");
    assert_eq!(forgotten["error"]["data"]["scope"], "memory.delete");

    // A token for another resource is refused at the transport, as a 401.
    let other = person_token(&live, "https://other-resource.invalid", &["memory.read"]).await;
    let reply = support::post(
        &cell,
        "/mcp",
        Some(&other),
        &[],
        &request(1, "ping", json!({})),
    )
    .await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
    assert!(
        reply.headers[header::WWW_AUTHENTICATE]
            .to_str()
            .unwrap()
            .contains("invalid_token")
    );

    // The anonymous GET is the challenge; the anonymous POST is the chassis's
    // CSRF refusal (020 D-10), which is the bootstrap's blocker.
    let anonymous_get = cell.http.get(cell.url("/mcp")).send().await.unwrap();
    assert_eq!(anonymous_get.status(), StatusCode::UNAUTHORIZED);
    let anonymous_post = support::post(
        &cell,
        "/mcp",
        None,
        &[],
        &request(1, "initialize", json!({})),
    )
    .await;
    eprintln!("anonymous POST /mcp answered {}", anonymous_post.status);

    if let Ok(sdk) = std::env::var("AICORTEX_MCP_SDK") {
        let url = cell.url("/mcp");
        let (code, out, err) = run_client(url.clone(), Some(token.clone()), sdk.clone()).await;
        assert_eq!(code, 0, "the SDK client with a token: {out} {err}");
        let summary: Value = serde_json::from_str(out.trim()).expect("one JSON line");
        eprintln!("sdk client with token: {summary}");
        assert_eq!(summary["tools"].as_array().unwrap().len(), 7);
        assert_eq!(summary["framed"], true);
        assert_eq!(summary["isError"], false);

        let (code, out, err) = run_client(url, None, sdk).await;
        eprintln!("sdk client anonymous: exit {code}: {out}{err}");
        assert_ne!(code, 0, "the bootstrap is not met at rahi 0.7.1 (020 D-10)");
    } else {
        eprintln!("AICORTEX_MCP_SDK is unset: the real-client run is skipped");
    }
    cell.shutdown().await;
}
