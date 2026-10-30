//! One error type, answered as an RFC 9457 problem document (B-7).
//!
//! Every refusal this crate writes has a stable reason code, and the `type`
//! URI is derived from that code alone, so a client branches on `type` (or
//! on `code`, the same fact) without parsing prose. A refusal from the write
//! gate is a 422 carrying the gate's reason code and nothing of the content
//! it refused (013 B-3).
//!
//! The chassis answers its own refusals (a missing credential, a missing
//! scope, a spent rate budget) before a request reaches this crate, with its
//! own body and the challenge headers of RFC 6750; nothing here restates them.

use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use rahi_types::Error;
use serde::Serialize;

/// The media type of a problem document.
pub const PROBLEM_JSON: &str = "application/problem+json";

/// The prefix every problem `type` carries; the reason code follows it.
pub const PROBLEM_TYPE_PREFIX: &str = "urn:aicortex:problem:";

/// The problem `type` URI for `code`.
#[must_use]
pub fn problem_type(code: &str) -> String {
    format!("{PROBLEM_TYPE_PREFIX}{code}")
}

/// A refusal or failure, as this surface reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    detail: String,
}

impl ApiError {
    /// An error with an explicit status, code, and human-readable detail.
    ///
    /// The detail never carries memory content: callers pass reason
    /// sentences, field names, and ids.
    #[must_use]
    pub fn new(status: StatusCode, code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            status,
            code,
            detail: detail.into(),
        }
    }

    /// 400: the request is malformed.
    #[must_use]
    pub fn invalid(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "invalid_request", detail)
    }

    /// 404: the scope or the memory is not one the caller can reach (B-2).
    ///
    /// One answer for "does not exist" and "exists but not yours", so scope
    /// existence does not leak (FR-004).
    #[must_use]
    pub fn not_found() -> Self {
        Self::new(
            StatusCode::NOT_FOUND,
            "not_found",
            "no such resource in a scope this principal can reach",
        )
    }

    /// 422: the write gate refused the candidate, with its reason code.
    #[must_use]
    pub fn refused(reason: &aicortex_gate::Reason) -> Self {
        Self::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            reason.code(),
            reason.sentence(),
        )
    }

    /// The HTTP status.
    #[must_use]
    pub const fn status(&self) -> StatusCode {
        self.status
    }

    /// The stable reason code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        self.code
    }

    /// The problem document this error answers with.
    #[must_use]
    pub fn problem(&self) -> Problem {
        Problem {
            kind: problem_type(self.code),
            title: self.status.canonical_reason().unwrap_or("Error").to_owned(),
            status: self.status.as_u16(),
            detail: self.detail.clone(),
            code: self.code,
        }
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}: {}", self.status.as_u16(), self.code, self.detail)
    }
}

impl std::error::Error for ApiError {}

/// The wire shape of RFC 9457, with the reason code as an extension member.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Problem {
    /// The stable URI naming the reason.
    #[serde(rename = "type")]
    pub kind: String,
    /// The status's reason phrase.
    pub title: String,
    /// The HTTP status, repeated in the body as RFC 9457 allows.
    pub status: u16,
    /// What happened, in prose. Never content.
    pub detail: String,
    /// The reason code `type` is derived from.
    pub code: &'static str,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = serde_json::to_vec(&self.problem()).unwrap_or_default();
        let mut response = (self.status, body).into_response();
        response
            .headers_mut()
            .insert(header::CONTENT_TYPE, HeaderValue::from_static(PROBLEM_JSON));
        response
    }
}

impl From<Error> for ApiError {
    /// The workspace error, mapped by kind.
    ///
    /// Integrity, I/O, and configuration failures are this cell failing, and
    /// their messages describe its internals, so the detail is replaced with
    /// a fixed sentence rather than echoed to a client.
    fn from(error: Error) -> Self {
        match error {
            Error::Validation(detail) => Self::invalid(detail),
            Error::NotFound(_) => Self::not_found(),
            Error::Conflict(detail) => Self::new(StatusCode::CONFLICT, "conflict", detail),
            Error::Denied(detail) => Self::new(StatusCode::FORBIDDEN, "denied", detail),
            Error::Unauthorized(detail) => {
                Self::new(StatusCode::UNAUTHORIZED, "unauthorized", detail)
            }
            Error::Stale(detail) => Self::new(StatusCode::CONFLICT, "stale", detail),
            Error::Upstream(_) => Self::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "unavailable",
                "a dependency of this cell is unavailable; retry later",
            ),
            Error::Integrity(_) | Error::Io(_) | Error::Config(_) => Self::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "this cell could not complete the request",
            ),
        }
    }
}

/// What a handler returns.
pub type ApiResult<T> = Result<T, ApiError>;
