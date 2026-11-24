//! The one endpoint: streamable HTTP (B-1, B-2, B-3, B-4).
//!
//! `POST` carries a JSON-RPC message and is answered as a JSON body or as a
//! one-event stream, by what the client's `Accept` offers; `GET` opens the
//! server-initiated stream, which carries nothing but the chassis's keep-alive
//! and its shutdown event because this server initiates no message (it offers
//! no prompts, sampling, or subscriptions). Every handler runs after the
//! chassis's bearer layer, so a principal is present or the answer is the 401
//! the layer turns into the discovery challenge (B-4).
//!
//! Handlers hold no state between requests (B-2): the revision comes from the
//! request's header, the principal from its token, and a session id is echoed.

use aicortex_api::Api;
use axum::Router;
use axum::body::to_bytes;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use rahi_edge::AppState;
use rahi_edge::stream::{self, Event, StreamHub, StreamRoutes};
use rahi_idp::Bearer;
use rahi_types::Principal;
use serde_json::{Value, json};

use crate::compat::{COMPAT_PATH, Shape};
use crate::render::{self, INVALID_REQUEST, JSONRPC, METHOD_NOT_FOUND, PARSE_ERROR, RpcError};
use crate::resources;
use crate::session::{self, Revision};
use crate::tools::{self, Plan};

/// Where the server is mounted (B-1).
pub const MCP_PATH: &str = "/mcp";

/// The largest request body this endpoint reads, in bytes.
pub const MAX_BODY_BYTES: usize = 1024 * 1024;

/// What a handler shares.
#[derive(Clone)]
pub struct Mcp {
    api: Router,
    app: AppState,
    shape: Shape,
}

/// The router the cell mounts: `/mcp`, and the search and fetch shape beside
/// it (B-7). Both are declared streaming routes (`rahi://026`), because either
/// answers as a stream when the client offers only `text/event-stream`.
pub fn router(api: Api) -> Router {
    let routed = aicortex_api::router_with(api.clone());
    let at = |shape| Mcp {
        api: routed.clone(),
        app: api.app.clone(),
        shape,
    };
    let standard = Router::new()
        .stream_route(MCP_PATH, post(handle_post).get(handle_get))
        .with_state(at(Shape::Standard));
    let compat = Router::new()
        .stream_route(COMPAT_PATH, post(handle_post).get(handle_get))
        .with_state(at(Shape::SearchFetch));
    standard.merge(compat)
}

/// What the client's `Accept` offers: `(json, event stream)`.
///
/// A missing header offers both. A wildcard offers what it covers. Quality
/// values are ignored: this answers in the one form the client names, and in
/// JSON when it names both.
fn offers(headers: &HeaderMap) -> (bool, bool) {
    let Some(accept) = headers.get(header::ACCEPT).and_then(|v| v.to_str().ok()) else {
        return (true, true);
    };
    let mut json = false;
    let mut sse = false;
    for part in accept.split(',') {
        let media = part
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        match media.as_str() {
            "application/json" | "application/*" => json = true,
            "text/event-stream" | "text/*" => sse = true,
            "*/*" => {
                json = true;
                sse = true;
            }
            _ => {}
        }
    }
    (json, sse)
}

fn unauthorized() -> Response {
    // The chassis's bearer layer attaches the `resource_metadata` challenge to
    // any 401 from a declared bearer route (B-4).
    let body = json!({
        "type": "urn:aicortex:problem:unauthorized",
        "title": "Unauthorized",
        "status": 401,
        "detail": "this request carries no credential",
        "code": "unauthorized",
    });
    let mut response = (StatusCode::UNAUTHORIZED, body.to_string()).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/problem+json"),
    );
    response
}

fn refusal(status: StatusCode, detail: &'static str) -> Response {
    (status, detail).into_response()
}

fn with_session(mut response: Response, headers: &HeaderMap) -> Response {
    if let Some((name, value)) = session::echo(headers) {
        response.headers_mut().insert(name, value);
    }
    response
}

/// A message as a JSON body.
fn json_response(status: StatusCode, message: &Value) -> Response {
    let mut response = (status, message.to_string()).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    response
}

/// A message as a stream of one event, then the stream ends.
fn event_response(mcp: &Mcp, message: &Value) -> Response {
    let (producer, receiver) = match mcp.app.extension::<StreamHub>() {
        Some(hub) => hub.channel(),
        None => stream::channel(stream::DEFAULT_CHANNEL_CAPACITY),
    };
    let _ = producer.emit(Event::new("message", message.to_string()));
    drop(producer);
    stream::stream(receiver)
}

/// Who is asking, as the chassis resolved it.
struct Who {
    principal: Principal,
    bearer: Option<Bearer>,
}

/// `POST`: one JSON-RPC message (B-1, B-3).
async fn handle_post(State(mcp): State<Mcp>, request: Request) -> Response {
    let (parts, body) = request.into_parts();
    let Some(principal) = parts.extensions.get::<Principal>().cloned() else {
        return unauthorized();
    };
    let who = Who {
        principal,
        bearer: parts.extensions.get::<Bearer>().cloned(),
    };
    let (json_ok, sse_ok) = offers(&parts.headers);
    if !json_ok && !sse_ok {
        return refusal(
            StatusCode::NOT_ACCEPTABLE,
            "offer application/json or text/event-stream in Accept",
        );
    }
    let respond = |status: StatusCode, message: &Value| {
        if json_ok || !sse_ok {
            json_response(status, message)
        } else {
            event_response(&mcp, message)
        }
    };

    let message = match to_bytes(body, MAX_BODY_BYTES).await {
        Ok(bytes) => serde_json::from_slice::<Value>(&bytes)
            .map_err(|_| RpcError::new(PARSE_ERROR, "the body is not JSON")),
        Err(_) => Err(RpcError::new(INVALID_REQUEST, "the body is too large")),
    };
    let response = match message {
        Err(error) => respond(StatusCode::OK, &render::error(&Value::Null, &error)),
        Ok(message) => match process(&mcp, &who, &parts.headers, message).await {
            Processed::Accepted => {
                let mut response = StatusCode::ACCEPTED.into_response();
                response.headers_mut().remove(header::CONTENT_TYPE);
                response
            }
            Processed::Reply(reply) => respond(StatusCode::OK, &reply),
        },
    };
    with_session(response, &parts.headers)
}

/// `GET`: the server-initiated stream (B-1).
///
/// It holds the connection open under the chassis's budget and keep-alive and
/// ends with its shutdown event. This server initiates no message, so nothing
/// else is ever sent on it.
async fn handle_get(State(mcp): State<Mcp>, request: Request) -> Response {
    if request.extensions().get::<Principal>().is_none() {
        return unauthorized();
    }
    let (_, sse_ok) = offers(request.headers());
    if !sse_ok {
        return refusal(
            StatusCode::NOT_ACCEPTABLE,
            "offer text/event-stream in Accept",
        );
    }
    let (producer, receiver) = match mcp.app.extension::<StreamHub>() {
        Some(hub) => hub.channel(),
        None => stream::channel(stream::DEFAULT_CHANNEL_CAPACITY),
    };
    tokio::spawn(async move { producer.cancelled().await });
    with_session(stream::stream(receiver), request.headers())
}

/// What one POSTed message came to.
enum Processed {
    /// A notification or a client's response: nothing to answer (202).
    Accepted,
    /// The JSON-RPC response, with HTTP 200 (B-3).
    Reply(Value),
}

async fn process(mcp: &Mcp, who: &Who, headers: &HeaderMap, message: Value) -> Processed {
    let reject = |error: RpcError| Processed::Reply(render::error(&Value::Null, &error));
    if message.is_array() {
        // 2025-06-18 removed batching, and a batch would multiply the one rate
        // limit count a request pays (B-9, D-3 of this build).
        return reject(RpcError::new(
            INVALID_REQUEST,
            "batched requests are not supported",
        ));
    }
    let Some(object) = message.as_object() else {
        return reject(RpcError::new(
            INVALID_REQUEST,
            "the message is not an object",
        ));
    };
    if object.get("jsonrpc").and_then(Value::as_str) != Some(JSONRPC) {
        return reject(RpcError::new(INVALID_REQUEST, "jsonrpc is not \"2.0\""));
    }
    let Some(method) = object.get("method").and_then(Value::as_str) else {
        // A response to a server request. This server sends none.
        return Processed::Accepted;
    };
    let Some(id) = object.get("id") else {
        return Processed::Accepted;
    };
    if !(id.is_string() || id.is_number()) {
        return reject(RpcError::new(INVALID_REQUEST, "id is a string or a number"));
    }
    let revision = match session::from_headers(headers) {
        Ok(revision) => revision,
        Err(offered) => {
            return Processed::Reply(render::error(
                id,
                &RpcError::new(INVALID_REQUEST, "unsupported protocol version").with_data(json!({
                    "offered": offered,
                    "supported": Revision::ALL.map(Revision::as_str),
                })),
            ));
        }
    };
    let params = object.get("params").cloned().unwrap_or(Value::Null);
    Processed::Reply(match dispatch(mcp, who, revision, method, params).await {
        Ok(result) => render::result(id, result),
        Err(error) => render::error(id, &error),
    })
}

async fn dispatch(
    mcp: &Mcp,
    who: &Who,
    revision: Revision,
    method: &str,
    params: Value,
) -> Result<Value, RpcError> {
    match method {
        "initialize" => {
            let requested = params.get("protocolVersion").and_then(Value::as_str);
            Ok(session::initialize_result(session::negotiate(requested)))
        }
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tools::definitions() })),
        "tools/call" => call_tool(mcp, who, revision, &params).await,
        "resources/list" => Ok(resources::list()),
        "resources/templates/list" => Ok(resources::templates()),
        "resources/read" => read_resource(mcp, who, &params).await,
        _ => Err(RpcError::new(METHOD_NOT_FOUND, "no such method")),
    }
}

async fn call_tool(
    mcp: &Mcp,
    who: &Who,
    revision: Revision,
    params: &Value,
) -> Result<Value, RpcError> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::invalid_params("name is the tool's name"))?;
    let arguments = params.get("arguments").cloned().unwrap_or(Value::Null);
    match tools::plan(name, arguments)? {
        Plan::NotYetAvailable => Err(tools::not_yet_available(name)),
        Plan::Route(upstream) => {
            let answer =
                tools::call(&mcp.api, &who.principal, who.bearer.as_ref(), upstream).await?;
            let (payload, is_error) = tools::interpret(answer)?;
            Ok(render::tool_result(revision, mcp.shape, payload, is_error))
        }
    }
}

async fn read_resource(mcp: &Mcp, who: &Who, params: &Value) -> Result<Value, RpcError> {
    let uri = params
        .get("uri")
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::invalid_params("uri names the resource"))?;
    let read = resources::parse(uri)?;
    let answer = tools::call(
        &mcp.api,
        &who.principal,
        who.bearer.as_ref(),
        read.upstream(),
    )
    .await?;
    let (stats, is_error) = tools::interpret(answer)?;
    if is_error {
        return Err(RpcError::new(
            render::RESOURCE_NOT_FOUND,
            "no such resource in a scope this principal can reach",
        )
        .with_data(json!({ "code": "resource_not_found" })));
    }
    Ok(resources::contents(uri, &read.document(&stats)))
}
