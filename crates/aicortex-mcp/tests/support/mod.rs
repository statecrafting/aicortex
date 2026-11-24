//! The cell the MCP tests run against: the API's own harness (spec 020's
//! `tests/common`, included by path so there is one) with the MCP router
//! mounted beside the API, the chassis's bearer layer in front, and real RS256
//! tokens. The authorization under test is the chassis's.

#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

#[path = "../../../aicortex-api/tests/common/mod.rs"]
pub mod common;

use aicortex_mcp::Revision;
#[allow(unused_imports)]
pub use common::{Cell, Who};
use reqwest::{StatusCode, header};
use serde_json::{Value, json};

/// Every scope a tool needs.
pub const ALL: &[&str] = &["memory.read", "memory.write", "memory.delete"];

/// A cell serving the API and the MCP server, composed as `serve` does.
pub async fn boot() -> Cell {
    boot_with(false).await
}

/// The same, with the edge letting an anonymous request reach a declared
/// bearer route, which the bootstrap of B-4 needs (see [`common::Extra`]).
pub async fn boot_open() -> Cell {
    boot_with(true).await
}

async fn boot_with(open_bootstrap: bool) -> Cell {
    common::boot_mounting(
        None,
        Box::new(move |api| common::Extra {
            router: aicortex_mcp::router(api.clone()),
            bearer: aicortex_mcp::bearer_routes(aicortex_api::bearer_routes()),
            open_bootstrap,
        }),
    )
    .await
}

/// What a POST to the server answered.
pub struct Reply {
    pub status: StatusCode,
    pub headers: header::HeaderMap,
    pub text: String,
}

impl Reply {
    /// The JSON-RPC message, from a JSON body or the first event of a stream.
    pub fn message(&self) -> Value {
        let text = self
            .text
            .lines()
            .find_map(|line| line.strip_prefix("data: "))
            .unwrap_or(&self.text);
        serde_json::from_str(text).unwrap_or_else(|_| panic!("a JSON-RPC message: {}", self.text))
    }
}

/// POST `body` to `path` with the stated headers and token.
pub async fn post(
    cell: &Cell,
    path: &str,
    token: Option<&str>,
    headers: &[(&str, &str)],
    body: &Value,
) -> Reply {
    let mut request = cell
        .http
        .post(cell.url(path))
        .header(header::CONTENT_TYPE, "application/json")
        .body(body.to_string());
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = request.send().await.expect("answered");
    Reply {
        status: response.status(),
        headers: response.headers().clone(),
        text: response.text().await.expect("a body"),
    }
}

/// A JSON-RPC request.
pub fn request(id: u64, method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
}

/// Call `method` as an established client of `revision`, answered as JSON.
pub async fn rpc(
    cell: &Cell,
    token: &str,
    revision: Revision,
    method: &str,
    params: Value,
) -> Value {
    let reply = post(
        cell,
        "/mcp",
        Some(token),
        &[
            ("accept", "application/json, text/event-stream"),
            ("mcp-protocol-version", revision.as_str()),
        ],
        &request(1, method, params),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    reply.message()
}

/// `tools/call` at the newest revision.
pub async fn call(cell: &Cell, token: &str, name: &str, arguments: Value) -> Value {
    rpc(
        cell,
        token,
        Revision::LATEST,
        "tools/call",
        json!({ "name": name, "arguments": arguments }),
    )
    .await
}

/// The structured content of a successful tool result.
pub fn structured(message: &Value) -> &Value {
    assert!(message.get("error").is_none(), "{message}");
    assert_eq!(message["result"]["isError"], json!(false), "{message}");
    &message["result"]["structuredContent"]
}
