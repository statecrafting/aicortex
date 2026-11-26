//! What named clients need that the protocol does not say (B-7).
//!
//! A documented list, not folklore: each [`Quirk`] names the client, the
//! behavior observed or documented by its vendor, and what this crate does
//! about it. An entry with no client and no behavior does not belong here.
//!
//! The one structural accommodation is the `search` and `fetch` pair. A client
//! that requires exactly that pair reads one text block holding a JSON
//! document, so [`Shape::SearchFetch`] renders the same two tools that way, on
//! its own path ([`COMPAT_PATH`]) so the standard surface is untouched and the
//! tool list stays the seven of D-1. The rendering keeps spec 019: a title is
//! never taken from stored text (it is derived from the id), and the whole
//! framed response, framing statement and envelope, is the `text` member.

use serde_json::{Value, json};

/// Where the search and fetch shape is mounted, beside `/mcp`.
pub const COMPAT_PATH: &str = "/mcp/compat/search-fetch";

/// One documented accommodation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Quirk {
    /// The client that needs it.
    pub client: &'static str,
    /// The behavior it has, and where that is documented.
    pub behavior: &'static str,
    /// What this crate does about it.
    pub accommodation: &'static str,
}

/// Every accommodation, one entry each.
pub const QUIRKS: [Quirk; 1] = [Quirk {
    client: "OpenAI ChatGPT connectors and deep research",
    behavior: "Documented by OpenAI for remote MCP servers: the server must expose a `search` \
tool taking one `query` string and a `fetch` tool taking one `id` string, each returning a single \
text content item whose text is a JSON document (`results` of `id`, `title`, `url` for search; \
`id`, `title`, `text`, `url`, `metadata` for fetch).",
    accommodation: "The same two tools served at COMPAT_PATH render their result as that \
document (Shape::SearchFetch), with the framed response as `text` and a title derived from the id.",
}];

/// How a tool result's text block is rendered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    /// The framed response, serialized as it is.
    Standard,
    /// The `search` and `fetch` document shape (see the module comment).
    SearchFetch,
}

impl Shape {
    /// The text block for `payload`, the API's framed response.
    #[must_use]
    pub fn text(self, payload: &Value) -> String {
        match self {
            Self::Standard => payload.to_string(),
            Self::SearchFetch => search_fetch_text(payload),
        }
    }
}

/// The URL a memory is cited by: its id, in this server's own scheme.
fn url(id: &str) -> String {
    format!("aicortex://memory/{id}")
}

/// A title that is derived from the id and never from stored text (019 B-6).
fn title(id: &str) -> String {
    format!("Memory {id}")
}

/// The ids of a response's `items`, which are bare identifiers by 019 B-6.
fn ids(payload: &Value) -> Vec<String> {
    payload
        .get("items")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("id").and_then(Value::as_str))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// The document a search or fetch result is rendered as.
///
/// One id is a fetch (`id`, `title`, `text`, `url`, `metadata`); anything else
/// is a search (`results`). `text` is the whole framed response serialized, so
/// the framing statement precedes the envelope inside it.
fn search_fetch_text(payload: &Value) -> String {
    let ids = ids(payload);
    let document = match ids.as_slice() {
        [id] => json!({
            "id": id,
            "title": title(id),
            "text": payload.to_string(),
            "url": url(id),
            "metadata": { "framing_version": payload.get("framing_version") },
        }),
        _ => json!({
            "results": ids
                .iter()
                .map(|id| json!({ "id": id, "title": title(id), "url": url(id) }))
                .collect::<Vec<_>>(),
        }),
    };
    document.to_string()
}

#[cfg(test)]
#[allow(clippy::indexing_slicing, clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn every_quirk_names_its_client_and_behavior() {
        for quirk in QUIRKS {
            assert!(!quirk.client.is_empty() && !quirk.behavior.is_empty());
            assert!(!quirk.accommodation.is_empty());
        }
    }

    #[test]
    fn a_single_item_renders_as_a_fetch_document_with_a_derived_title() {
        let payload = json!({
            "framing": "frame",
            "framing_version": 1,
            "memories": [],
            "items": [{ "id": "abc" }],
        });
        let text = Shape::SearchFetch.text(&payload);
        let document: Value = serde_json::from_str(&text).expect("a document");
        assert_eq!(document["id"], "abc");
        assert_eq!(document["title"], "Memory abc");
        assert_eq!(document["url"], "aicortex://memory/abc");
        let inner: Value = serde_json::from_str(document["text"].as_str().expect("text"))
            .expect("the framed response");
        assert_eq!(inner, payload);
    }

    #[test]
    fn several_items_render_as_search_results() {
        let payload = json!({ "items": [{ "id": "a" }, { "id": "b" }] });
        let document: Value =
            serde_json::from_str(&Shape::SearchFetch.text(&payload)).expect("a document");
        assert_eq!(document["results"].as_array().map(Vec::len), Some(2));
        assert_eq!(document["results"][1]["title"], "Memory b");
    }
}
