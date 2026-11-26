//! Resources (B-8): read-only counts, never a memory body.
//!
//! Some clients load resources into context on their own, which is an
//! instruction position under 019 B-3, so nothing here can carry stored text.
//! A scope summary is the route `GET /stats` answers, which is counters keyed
//! by the closed vocabularies of kinds and states, and the review queue count
//! is `GET /review/count`, which answers counts by the closed vocabulary of
//! review kinds (spec 023 B-2, 021 D-13). The summary of a scope the
//! caller cannot reach is the API's own 404, so existence does not leak.

use serde_json::{Value, json};

use crate::render::{RESOURCE_NOT_FOUND, RpcError};
use crate::tools::Upstream;

/// The scheme of every resource URI this server serves.
pub const SCHEME: &str = "aicortex://";
/// The review queue count.
pub const REVIEW_QUEUE_URI: &str = "aicortex://review-queue";
/// The caller's personal scope summary.
pub const PERSONAL_URI: &str = "aicortex://scopes/personal";

/// What `resources/list` answers: static entries.
#[must_use]
pub fn list() -> Value {
    json!({
        "resources": [
            {
                "uri": PERSONAL_URI,
                "name": "personal-scope-summary",
                "description": "Counts of memories in your personal scope by kind and state. Counts only.",
                "mimeType": "application/json",
            },
            {
                "uri": REVIEW_QUEUE_URI,
                "name": "review-queue-count",
                "description": "How many items await a person's review in your personal scope, by kind. A count only.",
                "mimeType": "application/json",
            },
        ],
    })
}

/// What `resources/templates/list` answers: the other scope kinds.
#[must_use]
pub fn templates() -> Value {
    json!({
        "resourceTemplates": [
            {
                "uriTemplate": "aicortex://scopes/project/{key}",
                "name": "project-scope-summary",
                "description": "Counts of memories in one of your project scopes. Counts only.",
                "mimeType": "application/json",
            },
            {
                "uriTemplate": "aicortex://scopes/shared/{key}",
                "name": "shared-scope-summary",
                "description": "Counts of memories in one of your shared scopes. Counts only.",
                "mimeType": "application/json",
            },
        ],
    })
}

/// What a resource URI asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Read {
    /// Counters for a scope.
    Summary {
        /// `personal`, `project`, or `shared`.
        kind: String,
        /// The key, for a project or shared scope.
        key: Option<String>,
    },
    /// The count of items waiting for a person in the personal scope.
    ReviewQueue,
}

/// Parse a resource URI.
///
/// # Errors
///
/// A resource-not-found error for a URI this server does not serve.
pub fn parse(uri: &str) -> Result<Read, RpcError> {
    let missing = || {
        RpcError::new(RESOURCE_NOT_FOUND, "no such resource")
            .with_data(json!({ "code": "resource_not_found" }))
    };
    let rest = uri.strip_prefix(SCHEME).ok_or_else(missing)?;
    match rest.split('/').collect::<Vec<_>>().as_slice() {
        ["review-queue"] => Ok(Read::ReviewQueue),
        ["scopes", "personal"] => Ok(Read::Summary {
            kind: "personal".to_owned(),
            key: None,
        }),
        ["scopes", kind @ ("project" | "shared"), key] if !key.is_empty() => Ok(Read::Summary {
            kind: (*kind).to_owned(),
            key: Some((*key).to_owned()),
        }),
        _ => Err(missing()),
    }
}

impl Read {
    /// The API route that answers it.
    #[must_use]
    pub fn upstream(&self) -> Upstream {
        let query = match self {
            Self::ReviewQueue => return Upstream::review_count(),
            Self::Summary { kind, key } => {
                let mut query = vec![("scope", kind.clone())];
                match (kind.as_str(), key) {
                    ("project", Some(key)) => query.push(("project", key.clone())),
                    ("shared", Some(key)) => query.push(("share", key.clone())),
                    _ => {}
                }
                query
            }
        };
        Upstream::stats(query)
    }

    /// The document the upstream answer becomes: counts only.
    #[must_use]
    pub fn document(&self, stats: &Value) -> Value {
        match self {
            Self::ReviewQueue => json!({
                "open": stats.get("open").and_then(Value::as_u64).unwrap_or(0),
                "by_kind": stats.get("by_kind").cloned().unwrap_or_else(|| json!({})),
            }),
            Self::Summary { kind, key } => json!({
                "scope": key.as_ref().map_or_else(|| kind.clone(), |key| format!("{kind}/{key}")),
                "total": stats.get("total").and_then(Value::as_u64).unwrap_or(0),
                "by_kind": stats.get("by_kind").cloned().unwrap_or_else(|| json!({})),
                "by_status": stats.get("by_status").cloned().unwrap_or_else(|| json!({})),
            }),
        }
    }
}

/// The `resources/read` result for `uri`.
#[must_use]
pub fn contents(uri: &str, document: &Value) -> Value {
    json!({
        "contents": [{ "uri": uri, "mimeType": "application/json", "text": document.to_string() }],
    })
}

#[cfg(test)]
#[allow(clippy::indexing_slicing, clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn uris_parse_to_the_three_kinds_and_refuse_the_rest() {
        assert_eq!(parse(REVIEW_QUEUE_URI), Ok(Read::ReviewQueue));
        assert!(matches!(parse(PERSONAL_URI), Ok(Read::Summary { .. })));
        assert!(parse("aicortex://scopes/project/alpha").is_ok());
        assert!(parse("aicortex://scopes/project/").is_err());
        assert!(parse("aicortex://memories/abc").is_err());
        assert!(parse("file:///etc/passwd").is_err());
    }
}
