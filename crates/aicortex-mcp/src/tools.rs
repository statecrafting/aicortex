//! The seven tools (B-5, B-9, D-1, D-3).
//!
//! Each tool is a translation: validated arguments become one request to the
//! API's own router (spec 020), made in process with the principal and bearer
//! the chassis already resolved, so the scope gate that decides is the
//! route's own and this crate holds no authorization logic and no SQL. The
//! answer is the route's framed JSON, handed to [`crate::render`].
//!
//! Descriptions and input schemas are static text authored here. Nothing a
//! store returns is ever interpolated into them (B-6, 019 B-3, FR-004).

use aicortex_api::{API_PREFIX, ApiScope};
use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Method, Request, StatusCode, header};
use rahi_idp::Bearer;
use rahi_types::Principal;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::render::{
    INTERNAL_ERROR, NOT_YET_AVAILABLE, NOT_YET_AVAILABLE_CODE, RpcError, UNAUTHORIZED,
};

/// Candidates one `capture` call may carry (B-9). Lower than the API's own
/// bound: a model's call is not an importer's batch.
pub const MAX_CAPTURE_BATCH: usize = 20;
/// The most results `search` returns (B-9).
pub const MAX_SEARCH_K: u32 = 20;
/// What `search` returns when `k` is absent.
pub const DEFAULT_SEARCH_K: u32 = 10;
/// The most rows one `list` page returns.
pub const MAX_LIST_LIMIT: u32 = 50;
/// The longest search query, in characters.
pub const MAX_QUERY_CHARS: usize = 2_000;
/// The longest entity id `relate` takes, in characters.
pub const MAX_ENTITY_CHARS: usize = 200;
/// The longest idempotency key, in bytes.
pub const MAX_IDEMPOTENCY_BYTES: usize = 128;
/// The ceiling on an API answer this crate will read back.
const MAX_ANSWER_BYTES: usize = 4 * 1024 * 1024;

/// One tool: its name, the scope its route requires, and whether this build
/// serves it.
#[derive(Clone, Copy, Debug)]
struct Tool {
    name: &'static str,
    scope: ApiScope,
    served: bool,
}

/// The seven tools of D-1, in the order `tools/list` returns them. A test
/// fails if an eighth is added without amending the spec (FR-006).
const TOOLS: [Tool; 7] = [
    Tool {
        name: "capture",
        scope: ApiScope::MemoryWrite,
        served: true,
    },
    Tool {
        name: "search",
        scope: ApiScope::MemoryRead,
        served: true,
    },
    Tool {
        name: "fetch",
        scope: ApiScope::MemoryRead,
        served: true,
    },
    Tool {
        name: "list",
        scope: ApiScope::MemoryRead,
        served: true,
    },
    Tool {
        name: "update",
        scope: ApiScope::MemoryWrite,
        served: true,
    },
    Tool {
        name: "forget",
        scope: ApiScope::MemoryDelete,
        served: true,
    },
    Tool {
        name: "relate",
        scope: ApiScope::MemoryRead,
        served: true,
    },
];

/// The tool names, in list order.
#[must_use]
pub fn names() -> Vec<&'static str> {
    TOOLS.iter().map(|tool| tool.name).collect()
}

/// The scope each tool's route requires, by tool name.
#[must_use]
pub fn scope_of(name: &str) -> Option<&'static str> {
    TOOLS
        .iter()
        .find(|tool| tool.name == name)
        .map(|tool| tool.scope.as_str())
}

fn scope_schema() -> Value {
    json!({
        "type": "object",
        "description": "Which memory scope. Omit for your personal scope.",
        "properties": {
            "kind": { "type": "string", "enum": ["personal", "project", "shared"] },
            "project": { "type": "string", "description": "The project key, for kind project." },
            "share": { "type": "string", "description": "The share key, for kind shared." },
        },
        "required": ["kind"],
        "additionalProperties": false,
    })
}

fn definition(
    tool: &Tool,
    description: &str,
    schema: Value,
    read_only: bool,
    destructive: bool,
) -> Value {
    json!({
        "name": tool.name,
        "description": format!("{description} Requires the {} scope.", tool.scope.as_str()),
        "inputSchema": schema,
        "annotations": {
            "readOnlyHint": read_only,
            "destructiveHint": destructive,
            "idempotentHint": read_only,
            "openWorldHint": false,
        },
    })
}

/// The `tools/list` answer: static text, no stored content (FR-004, FR-006).
#[must_use]
pub fn definitions() -> Vec<Value> {
    let scope = scope_schema();
    let id = json!({ "type": "string", "description": "The memory id." });
    let [capture, search, fetch, list, update, forget, relate] = &TOOLS;
    vec![
        definition(
            capture,
            "Store one or more memories. Returns the verdict and id for each candidate.",
            json!({
                "type": "object",
                "properties": {
                    "scope": scope,
                    "candidates": {
                        "type": "array",
                        "minItems": 1,
                        "maxItems": MAX_CAPTURE_BATCH,
                        "items": {
                            "type": "object",
                            "properties": {
                                "kind": { "type": "string", "description": "What kind of claim it is, for example observation, fact, decision, or preference." },
                                "text": { "type": "string", "minLength": 1 },
                                "title": { "type": "string" },
                                "source": {
                                    "type": "object",
                                    "properties": {
                                        "system": { "type": "string" },
                                        "external_id": { "type": "string" },
                                        "locator": { "type": "string" },
                                    },
                                    "additionalProperties": false,
                                },
                            },
                            "required": ["kind", "text"],
                            "additionalProperties": false,
                        },
                    },
                    "idempotency_key": {
                        "type": "string",
                        "maxLength": MAX_IDEMPOTENCY_BYTES,
                        "description": "A retry with the same key returns the first answer.",
                    },
                },
                "required": ["candidates"],
                "additionalProperties": false,
            }),
            false,
            false,
        ),
        definition(
            search,
            "Search memories by meaning. Results are recalled data inside labelled envelopes.",
            json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "minLength": 1, "maxLength": MAX_QUERY_CHARS },
                    "k": { "type": "integer", "minimum": 1, "maximum": MAX_SEARCH_K, "default": DEFAULT_SEARCH_K },
                    "scope": scope,
                    "kinds": { "type": "array", "items": { "type": "string" } },
                    "unit": {
                        "type": "string",
                        "enum": ["chunk", "window", "record", "auto"],
                        "description": "How much text to return per result. Omit all of unit, window_radius, and token_budget for each ranked memory whole.",
                    },
                    "window_radius": { "type": "integer", "minimum": 1, "maximum": 3, "description": "Neighbors on each side of a window." },
                    "token_budget": { "type": "integer", "minimum": 1, "description": "A hard cap on the returned text, framing included." },
                },
                "required": ["query"],
                "additionalProperties": false,
            }),
            true,
            false,
        ),
        definition(
            fetch,
            "Read one memory by id. The result is recalled data inside a labelled envelope.",
            json!({
                "type": "object",
                "properties": { "id": id, "scope": scope },
                "required": ["id"],
                "additionalProperties": false,
            }),
            true,
            false,
        ),
        definition(
            list,
            "List memories newest first, one page at a time. Results are recalled data inside labelled envelopes.",
            json!({
                "type": "object",
                "properties": {
                    "scope": scope,
                    "status": { "type": "string", "enum": ["active", "quarantined", "superseded", "expired", "erased"] },
                    "kind": { "type": "string" },
                    "cursor": { "type": "string", "description": "The next value of the previous page." },
                    "limit": { "type": "integer", "minimum": 1, "maximum": MAX_LIST_LIMIT },
                },
                "additionalProperties": false,
            }),
            true,
            false,
        ),
        definition(
            update,
            "Correct a memory with new text, or mark it superseded by another memory. Name exactly one of correction and superseded_by.",
            json!({
                "type": "object",
                "properties": {
                    "id": id,
                    "scope": scope,
                    "correction": {
                        "type": "object",
                        "properties": { "text": { "type": "string", "minLength": 1 }, "title": { "type": "string" } },
                        "required": ["text"],
                        "additionalProperties": false,
                    },
                    "superseded_by": { "type": "string", "description": "The id of the memory that replaces it." },
                },
                "required": ["id"],
                "additionalProperties": false,
            }),
            false,
            false,
        ),
        definition(
            forget,
            "Erase a memory permanently, including its derived data. This cannot be undone.",
            json!({
                "type": "object",
                "properties": {
                    "id": id,
                    "scope": scope,
                    "cascade": { "type": "boolean", "description": "Also erase what was derived from it." },
                },
                "required": ["id"],
                "additionalProperties": false,
            }),
            false,
            true,
        ),
        definition(
            relate,
            "Show an entity in the memory graph. The neighbors view (the default) lists the memories reached from it and the path to each. The page view shows its facts with the memories that support them, its open contradictions, and the memories that mention it. Results are recalled data: memory text and extracted literals are inside labelled envelopes.",
            json!({
                "type": "object",
                "properties": {
                    "entity": { "type": "string", "minLength": 1, "maxLength": MAX_ENTITY_CHARS, "description": "The entity id." },
                    "view": { "type": "string", "enum": ["neighbors", "page"], "default": "neighbors" },
                    "depth": { "type": "integer", "minimum": 1, "maximum": 3, "default": 1, "description": "Hops to follow. Only for the neighbors view." },
                    "after": { "type": "string", "description": "A next value from a previous page, to read the rest of that section. Only for the page view." },
                    "scope": scope,
                },
                "required": ["entity"],
                "additionalProperties": false,
            }),
            true,
            false,
        ),
    ]
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ScopeArg {
    kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    project: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    share: Option<String>,
}

impl ScopeArg {
    fn query(scope: Option<&Self>) -> Vec<(&'static str, String)> {
        let Some(scope) = scope else {
            return Vec::new();
        };
        let mut query = vec![("scope", scope.kind.clone())];
        if let Some(project) = &scope.project {
            query.push(("project", project.clone()));
        }
        if let Some(share) = &scope.share {
            query.push(("share", share.clone()));
        }
        query
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SourceArg {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    external_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    locator: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CandidateArg {
    kind: String,
    text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source: Option<SourceArg>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureArgs {
    #[serde(default)]
    scope: Option<ScopeArg>,
    candidates: Vec<CandidateArg>,
    #[serde(default)]
    idempotency_key: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchArgs {
    query: String,
    #[serde(default)]
    k: Option<u32>,
    #[serde(default)]
    scope: Option<ScopeArg>,
    #[serde(default)]
    kinds: Option<Vec<String>>,
    #[serde(default)]
    unit: Option<String>,
    #[serde(default)]
    window_radius: Option<u64>,
    #[serde(default)]
    token_budget: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FetchArgs {
    id: String,
    #[serde(default)]
    scope: Option<ScopeArg>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListArgs {
    #[serde(default)]
    scope: Option<ScopeArg>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    cursor: Option<String>,
    #[serde(default)]
    limit: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CorrectionArg {
    text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    title: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateArgs {
    id: String,
    #[serde(default)]
    scope: Option<ScopeArg>,
    #[serde(default)]
    correction: Option<CorrectionArg>,
    #[serde(default)]
    superseded_by: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ForgetArgs {
    id: String,
    #[serde(default)]
    scope: Option<ScopeArg>,
    #[serde(default)]
    cascade: bool,
}

/// What `relate` shows of an entity (056 B-10).
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum RelateView {
    /// The memories reached from the entity, with their paths (017 B-9).
    #[default]
    Neighbors,
    /// The entity page (056).
    Page,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RelateArgs {
    entity: String,
    #[serde(default)]
    view: RelateView,
    #[serde(default)]
    depth: Option<u32>,
    #[serde(default)]
    after: Option<String>,
    #[serde(default)]
    scope: Option<ScopeArg>,
}

/// One request to the API's router.
#[derive(Debug)]
pub struct Upstream {
    method: Method,
    path: String,
    query: Vec<(&'static str, String)>,
    body: Option<Value>,
    idempotency_key: Option<String>,
}

impl Upstream {
    fn get(path: impl Into<String>, query: Vec<(&'static str, String)>) -> Self {
        Self {
            method: Method::GET,
            path: path.into(),
            query,
            body: None,
            idempotency_key: None,
        }
    }

    /// `GET /stats`, which resources read counts from (B-8).
    #[must_use]
    pub fn stats(query: Vec<(&'static str, String)>) -> Self {
        Self::get("/stats", query)
    }

    /// The route this call goes to, for tests.
    #[must_use]
    pub fn route(&self) -> String {
        format!("{} {}", self.method, self.path)
    }

    fn uri(&self) -> String {
        let mut uri = format!("{API_PREFIX}{}", self.path);
        for (index, (key, value)) in self.query.iter().enumerate() {
            uri.push(if index == 0 { '?' } else { '&' });
            uri.push_str(key);
            uri.push('=');
            uri.push_str(&percent_encode(value));
        }
        uri
    }
}

/// What a validated tool call becomes.
#[derive(Debug)]
pub enum Plan {
    /// A route of the API.
    Route(Upstream),
    /// A listed tool whose backing route has not landed (021 D-3).
    NotYetAvailable,
}

/// Percent-encode everything but RFC 3986 unreserved characters.
fn percent_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn arguments<T: for<'de> Deserialize<'de>>(arguments: Value) -> Result<T, RpcError> {
    let arguments = if arguments.is_null() {
        json!({})
    } else {
        arguments
    };
    serde_json::from_value(arguments).map_err(|error| {
        RpcError::invalid_params(format!(
            "the arguments do not match the tool's schema: {error}"
        ))
    })
}

fn bounded(name: &str, value: Option<u32>, max: u32) -> Result<Option<u32>, RpcError> {
    match value {
        Some(0) => Err(RpcError::invalid_params(format!("{name} is at least 1"))),
        Some(value) if value > max => {
            Err(RpcError::invalid_params(format!("{name} is at most {max}")))
        }
        other => Ok(other),
    }
}

fn nonempty(name: &str, text: &str, max_chars: usize) -> Result<(), RpcError> {
    if text.trim().is_empty() {
        return Err(RpcError::invalid_params(format!(
            "{name} must not be empty"
        )));
    }
    if text.chars().count() > max_chars {
        return Err(RpcError::invalid_params(format!(
            "{name} is at most {max_chars} characters"
        )));
    }
    Ok(())
}

fn memory_path(id: &str) -> Result<String, RpcError> {
    nonempty("id", id, 64)?;
    Ok(format!("/memories/{}", percent_encode(id)))
}

/// Validate `arguments` against the tool's schema and plan the call (B-9).
///
/// Everything here runs before any work: a malformed or oversized call is a
/// JSON-RPC invalid-params error and reaches no route.
///
/// # Errors
///
/// [`RpcError`] with code `-32602` for a call that does not match its schema,
/// or `-32601`-shaped unknown tool.
pub fn plan(name: &str, args: Value) -> Result<Plan, RpcError> {
    let tool = TOOLS
        .iter()
        .find(|tool| tool.name == name)
        .ok_or_else(|| RpcError::invalid_params("no such tool"))?;
    let plan = match tool.name {
        "capture" => {
            let args: CaptureArgs = arguments(args)?;
            if args.candidates.is_empty() || args.candidates.len() > MAX_CAPTURE_BATCH {
                return Err(RpcError::invalid_params(format!(
                    "candidates holds between 1 and {MAX_CAPTURE_BATCH} items"
                )));
            }
            if args.candidates.iter().any(|c| c.text.trim().is_empty()) {
                return Err(RpcError::invalid_params(
                    "a candidate's text must not be empty",
                ));
            }
            if let Some(key) = &args.idempotency_key
                && (key.is_empty() || key.len() > MAX_IDEMPOTENCY_BYTES || !key.is_ascii())
            {
                return Err(RpcError::invalid_params(
                    "idempotency_key is 1 to 128 ASCII bytes",
                ));
            }
            let mut body = json!({ "candidates": args.candidates });
            if let (Some(scope), Some(object)) = (&args.scope, body.as_object_mut()) {
                object.insert("scope".to_owned(), json!(scope));
            }
            Plan::Route(Upstream {
                method: Method::POST,
                path: "/capture".to_owned(),
                query: Vec::new(),
                body: Some(body),
                idempotency_key: args.idempotency_key,
            })
        }
        "search" => {
            let args: SearchArgs = arguments(args)?;
            nonempty("query", &args.query, MAX_QUERY_CHARS)?;
            bounded("k", args.k, MAX_SEARCH_K)?;
            let mut body = serde_json::Map::new();
            body.insert("query".to_owned(), json!(args.query));
            if let Some(k) = args.k {
                body.insert("k".to_owned(), json!(k));
            }
            if let Some(scope) = &args.scope {
                body.insert("scope".to_owned(), json!(scope));
            }
            if let Some(kinds) = &args.kinds {
                body.insert("filter".to_owned(), json!({ "kinds": kinds }));
            }
            // The route validates the delivery parameters, `turn` as a unit
            // included (025 B-1, FR-011), so there is one place that decides.
            for (name, value) in [
                ("unit", args.unit.map(Value::from)),
                ("window_radius", args.window_radius.map(Value::from)),
                ("token_budget", args.token_budget.map(Value::from)),
            ] {
                if let Some(value) = value {
                    body.insert(name.to_owned(), value);
                }
            }
            Plan::Route(Upstream {
                method: Method::POST,
                path: "/search".to_owned(),
                query: Vec::new(),
                body: Some(Value::Object(body)),
                idempotency_key: None,
            })
        }
        "fetch" => {
            let args: FetchArgs = arguments(args)?;
            Plan::Route(Upstream::get(
                memory_path(&args.id)?,
                ScopeArg::query(args.scope.as_ref()),
            ))
        }
        "list" => {
            let args: ListArgs = arguments(args)?;
            bounded("limit", args.limit, MAX_LIST_LIMIT)?;
            let mut query = ScopeArg::query(args.scope.as_ref());
            if let Some(status) = args.status {
                query.push(("status", status));
            }
            if let Some(kind) = args.kind {
                query.push(("kind", kind));
            }
            if let Some(cursor) = args.cursor {
                query.push(("cursor", cursor));
            }
            if let Some(limit) = args.limit {
                query.push(("limit", limit.to_string()));
            }
            Plan::Route(Upstream::get("/memories", query))
        }
        "update" => {
            let args: UpdateArgs = arguments(args)?;
            if args.correction.is_some() == args.superseded_by.is_some() {
                return Err(RpcError::invalid_params(
                    "name exactly one of correction and superseded_by",
                ));
            }
            if let Some(correction) = &args.correction {
                nonempty("correction.text", &correction.text, usize::MAX)?;
            }
            let mut body = serde_json::Map::new();
            if let Some(scope) = &args.scope {
                body.insert("scope".to_owned(), json!(scope));
            }
            if let Some(correction) = &args.correction {
                body.insert("correction".to_owned(), json!(correction));
            }
            if let Some(next) = &args.superseded_by {
                body.insert("superseded_by".to_owned(), json!(next));
            }
            Plan::Route(Upstream {
                method: Method::PATCH,
                path: memory_path(&args.id)?,
                query: Vec::new(),
                body: Some(Value::Object(body)),
                idempotency_key: None,
            })
        }
        "forget" => {
            let args: ForgetArgs = arguments(args)?;
            let mut query = ScopeArg::query(args.scope.as_ref());
            if args.cascade {
                query.push(("cascade", "true".to_owned()));
            }
            Plan::Route(Upstream {
                method: Method::DELETE,
                path: memory_path(&args.id)?,
                query,
                body: None,
                idempotency_key: None,
            })
        }
        _ => {
            let args: RelateArgs = arguments(args)?;
            nonempty("entity", &args.entity, MAX_ENTITY_CHARS)?;
            let mut query = ScopeArg::query(args.scope.as_ref());
            let path = format!("/entities/{}", percent_encode(&args.entity));
            match args.view {
                RelateView::Neighbors => {
                    if args.after.is_some() {
                        return Err(RpcError::invalid_params(
                            "after resumes a page section; it applies to view page",
                        ));
                    }
                    let depth = args.depth.unwrap_or(1);
                    if !(1..=3).contains(&depth) {
                        return Err(RpcError::invalid_params("depth is between 1 and 3"));
                    }
                    query.push(("depth", depth.to_string()));
                    Plan::Route(Upstream::get(format!("{path}/neighbors"), query))
                }
                RelateView::Page => {
                    if args.depth.is_some() {
                        return Err(RpcError::invalid_params(
                            "depth follows edges; it applies to view neighbors",
                        ));
                    }
                    if let Some(after) = args.after {
                        query.push(("after", after));
                    }
                    Plan::Route(Upstream::get(path, query))
                }
            }
        }
    };
    debug_assert!(tool.served == matches!(plan, Plan::Route(_)));
    Ok(plan)
}

/// An API answer: the status, the challenge header, and the parsed body.
#[derive(Debug)]
pub struct Answer {
    status: StatusCode,
    challenge: Option<String>,
    body: Value,
}

impl Answer {
    /// The HTTP status the route answered.
    #[must_use]
    pub const fn status(&self) -> StatusCode {
        self.status
    }
}

/// Make `upstream` against the API's router as the principal the chassis
/// resolved (B-5): the scope gate on the route is the one that decides.
///
/// # Errors
///
/// [`RpcError`] when the router cannot be called or its answer cannot be
/// read, which is this cell failing and says nothing about the request.
pub async fn call(
    api: &Router,
    principal: &Principal,
    bearer: Option<&Bearer>,
    upstream: Upstream,
) -> Result<Answer, RpcError> {
    let internal = || RpcError::new(INTERNAL_ERROR, "this cell could not complete the request");
    let mut builder = Request::builder()
        .method(upstream.method.clone())
        .uri(upstream.uri());
    let body = match &upstream.body {
        Some(body) => {
            builder = builder.header(header::CONTENT_TYPE, "application/json");
            Body::from(body.to_string())
        }
        None => Body::empty(),
    };
    if let Some(key) = &upstream.idempotency_key {
        builder = builder.header("idempotency-key", key.as_str());
    }
    let mut request = builder.body(body).map_err(|_| internal())?;
    request.extensions_mut().insert(principal.clone());
    if let Some(bearer) = bearer {
        request.extensions_mut().insert(bearer.clone());
    }
    let response = api
        .clone()
        .oneshot(request)
        .await
        .map_err(|never| match never {})?;
    let status = response.status();
    let challenge = header_text(response.headers(), header::WWW_AUTHENTICATE.as_str());
    let bytes = axum::body::to_bytes(response.into_body(), MAX_ANSWER_BYTES)
        .await
        .map_err(|_| internal())?;
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    Ok(Answer {
        status,
        challenge,
        body,
    })
}

fn header_text(headers: &HeaderMap, name: &str) -> Option<String> {
    headers.get(name)?.to_str().ok().map(str::to_owned)
}

/// The scope a challenge names, `scope="memory.delete"`.
fn challenged_scope(challenge: &str) -> Option<&str> {
    let (_, rest) = challenge.split_once("scope=\"")?;
    rest.split_once('"').map(|(scope, _)| scope)
}

/// What an answer means to the client (B-3).
///
/// An authorization failure, a malformed call, and a cell failure are
/// protocol-level and become JSON-RPC errors (still HTTP 200). A route's own
/// outcome (not found, a gate refusal, a conflict) is a tool result with
/// `isError`, carrying the problem document, which never holds content, so a
/// model can read it and correct itself.
///
/// # Errors
///
/// The [`RpcError`] for a protocol-level failure.
pub fn interpret(answer: Answer) -> Result<(Value, bool), RpcError> {
    let status = answer.status;
    if status.is_success() {
        return Ok((answer.body, false));
    }
    let code = answer
        .body
        .get("code")
        .and_then(Value::as_str)
        .map(str::to_owned);
    match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
            let scope = answer.challenge.as_deref().and_then(challenged_scope);
            let kind = if status == StatusCode::FORBIDDEN {
                "insufficient_scope"
            } else {
                "unauthorized"
            };
            Err(RpcError::new(
                UNAUTHORIZED,
                "the credential does not reach what this tool needs",
            )
            .with_data(json!({ "code": kind, "scope": scope })))
        }
        StatusCode::BAD_REQUEST => Err(RpcError::invalid_params(
            answer
                .body
                .get("detail")
                .and_then(Value::as_str)
                .unwrap_or("the arguments were refused"),
        )
        .with_data(json!({ "code": code }))),
        status if status.is_server_error() && status != StatusCode::NOT_IMPLEMENTED => Err(
            RpcError::new(INTERNAL_ERROR, "this cell could not complete the request")
                .with_data(json!({ "code": code })),
        ),
        _ => Ok((answer.body, true)),
    }
}

/// The error a listed tool answers until its route lands (021 D-3).
#[must_use]
pub fn not_yet_available(name: &str) -> RpcError {
    RpcError::new(
        NOT_YET_AVAILABLE,
        "this tool is listed and not available in this build",
    )
    .with_data(json!({ "code": NOT_YET_AVAILABLE_CODE, "tool": name }))
}
