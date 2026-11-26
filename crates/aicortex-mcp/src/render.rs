//! JSON-RPC envelopes and tool results (B-3, B-6).
//!
//! Everything a client reads leaves through this module, so the two rules of
//! the wire live in one place: a protocol error is a JSON-RPC error object
//! (the HTTP status stays 200 for the transport to decide, B-3), and a tool
//! result carries the API's framed response as structured content with the
//! trace summary moved into metadata (B-6). Nothing here reads a memory body:
//! the values it handles are the already framed responses of spec 020, which
//! reached them through spec 019's one envelope function.

use serde_json::{Map, Value, json};

use crate::compat::Shape;
use crate::session::Revision;

/// The JSON-RPC version string.
pub const JSONRPC: &str = "2.0";

/// The body is not JSON.
pub const PARSE_ERROR: i64 = -32700;
/// The body is JSON but not a request this server accepts.
pub const INVALID_REQUEST: i64 = -32600;
/// No such method.
pub const METHOD_NOT_FOUND: i64 = -32601;
/// The parameters do not match the method's schema.
pub const INVALID_PARAMS: i64 = -32602;
/// This cell failed.
pub const INTERNAL_ERROR: i64 = -32603;
/// The credential does not reach what the call needs (B-3). `data.code` says
/// which: `unauthorized` or `insufficient_scope`.
pub const UNAUTHORIZED: i64 = -32001;
/// A resource URI this server does not serve (the MCP resource convention).
pub const RESOURCE_NOT_FOUND: i64 = -32002;
/// The tool is listed with its final schema but its backing route has not
/// landed (021 D-3). `data.code` is [`NOT_YET_AVAILABLE_CODE`].
pub const NOT_YET_AVAILABLE: i64 = -32010;
/// The stable code of [`NOT_YET_AVAILABLE`].
pub const NOT_YET_AVAILABLE_CODE: &str = "not_yet_available";

/// A JSON-RPC error: a code, a fixed sentence, and optional machine data.
///
/// The message is authored in this crate and the data carries codes, scope
/// names, and ids, never stored content.
#[derive(Clone, Debug, PartialEq)]
pub struct RpcError {
    /// The JSON-RPC code.
    pub code: i64,
    /// What happened, in prose.
    pub message: String,
    /// Machine-readable detail.
    pub data: Option<Value>,
}

impl RpcError {
    /// An error with no data.
    #[must_use]
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    /// The same, with `data`.
    #[must_use]
    pub fn with_data(mut self, data: Value) -> Self {
        self.data = Some(data);
        self
    }

    /// The parameters are wrong (B-9).
    #[must_use]
    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self::new(INVALID_PARAMS, message)
    }
}

/// A success response for request `id`.
#[must_use]
pub fn result(id: &Value, result: Value) -> Value {
    json!({ "jsonrpc": JSONRPC, "id": id, "result": result })
}

/// An error response for request `id` (`null` when the id is unknown).
#[must_use]
pub fn error(id: &Value, error: &RpcError) -> Value {
    let mut object = Map::new();
    object.insert("code".to_owned(), json!(error.code));
    object.insert("message".to_owned(), json!(error.message));
    if let Some(data) = &error.data {
        object.insert("data".to_owned(), data.clone());
    }
    json!({ "jsonrpc": JSONRPC, "id": id, "error": Value::Object(object) })
}

/// The `_meta` key a result's trace summary travels under (B-6).
pub const META_TRACE: &str = "aicortex/trace";

/// A tool result (B-6).
///
/// `payload` is the API's framed response. The text block always carries it,
/// because every revision has text content and some clients read nothing
/// else; structured content carries the same value from the revision that
/// has it. A `trace` member is moved into metadata so the structured content
/// is the envelopes and the framing statement and nothing else.
#[must_use]
pub fn tool_result(revision: Revision, shape: Shape, mut payload: Value, is_error: bool) -> Value {
    let trace = payload
        .as_object_mut()
        .and_then(|object| object.remove("trace"));
    let text = shape.text(&payload);
    let mut object = Map::new();
    object.insert(
        "content".to_owned(),
        json!([{ "type": "text", "text": text }]),
    );
    if revision.has_structured_content() && shape == Shape::Standard {
        object.insert("structuredContent".to_owned(), payload);
    }
    object.insert("isError".to_owned(), json!(is_error));
    if let Some(trace) = trace {
        object.insert("_meta".to_owned(), json!({ META_TRACE: trace }));
    }
    Value::Object(object)
}

#[cfg(test)]
#[allow(clippy::indexing_slicing, clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn a_trace_moves_to_metadata_and_structured_content_follows_the_revision() {
        let payload = json!({ "framing": "f", "memories": [], "trace": { "query": "q" } });
        let new = tool_result(Revision::V20250618, Shape::Standard, payload.clone(), false);
        assert_eq!(new["_meta"][META_TRACE]["query"], "q");
        assert!(new["structuredContent"].get("trace").is_none());
        assert_eq!(new["isError"], false);
        let old = tool_result(Revision::V20250326, Shape::Standard, payload, false);
        assert!(old.get("structuredContent").is_none());
        assert!(
            old["content"][0]["text"]
                .as_str()
                .is_some_and(|t| !t.contains("trace"))
        );
    }

    #[test]
    fn an_error_envelope_carries_its_data_only_when_it_has_some() {
        let plain = error(&json!(1), &RpcError::new(INVALID_REQUEST, "no"));
        assert!(plain["error"].get("data").is_none());
        let rich = error(
            &json!(1),
            &RpcError::new(UNAUTHORIZED, "no").with_data(json!({ "code": "x" })),
        );
        assert_eq!(rich["error"]["data"]["code"], "x");
    }
}
