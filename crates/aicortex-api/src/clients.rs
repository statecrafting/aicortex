//! `GET /api/v1/clients/{name}`: the connection snippet for one client, with
//! this deployment's URL filled in (spec 022 B-1, B-3, D-1).
//!
//! The matrix and the snippet templates are the files under `clients/`,
//! embedded at build time, so the served text and the repository cannot
//! drift. This module knows no client by behavior: a client is a row in the
//! matrix and a template beside it. The match in [`template`] only maps a row
//! to its embedded file, and the test refuses a row with no template or a
//! template with no row.
//!
//! The matrix is the subset of TOML this module reads: `[client.<name>]`
//! tables of strings, booleans, and one-line string lists. The crate carries
//! no TOML dependency (spec 010 owns the dependency set), and a file outside
//! the subset is refused with the line, not guessed at.

use std::collections::BTreeMap;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::Serialize;

use crate::error::{ApiError, ApiResult};
use crate::router::{Api, Caller};

/// Where the MCP server is mounted. Spec 021 owns the path (`aicortex_mcp::MCP_PATH`);
/// this crate sits below it and cannot import it, so the value is repeated
/// (spec 022 D-7).
pub const MCP_PATH: &str = "/mcp";

/// The shipped matrix.
pub const MATRIX: &str = include_str!("../../../clients/matrix.toml");

/// The scope a capture hook may never hold (B-6).
pub const PROMOTE_SCOPE: &str = "memory.promote";

/// The shipped snippet template for `name`, or `None` when there is none.
#[must_use]
pub fn template(name: &str) -> Option<&'static str> {
    Some(match name {
        "claude-code" => include_str!("../../../clients/claude-code/snippet.txt"),
        "codex" => include_str!("../../../clients/codex/snippet.txt"),
        "cursor" => include_str!("../../../clients/cursor/snippet.txt"),
        "antigravity" => include_str!("../../../clients/antigravity/snippet.txt"),
        _ => return None,
    })
}

/// One row of the matrix (B-1).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Row {
    /// Transports the client supports for a remote server.
    pub transport: Vec<String>,
    /// Whether it signs in with OAuth against the server.
    pub oauth: bool,
    /// Whether it registers itself (dynamic client registration).
    pub dynamic_registration: bool,
    /// Whether it needs the compatibility pair of 021 B-7.
    pub compat_pair: bool,
    /// Its automatic capture mechanism, in a phrase.
    pub capture: String,
    /// The scopes capture requests; never `memory.promote` (B-6).
    pub capture_scopes: Vec<String>,
    /// Whether the row was verified against a release (FR-002).
    pub verified: bool,
    /// The minimum version verified, when verified.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_version: Option<String>,
    /// The date it was verified (`YYYY-MM-DD`), when verified.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verified_on: Option<String>,
    /// The fixture directory under `clients/`.
    pub fixtures: String,
    /// What is unverified or caveated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// The client's per-turn hook mechanism that could carry entity
    /// pointers, or `none` (026 B-11).
    pub read_pointer: String,
    /// Where the client places that hook's output (026 B-11).
    pub read_pointer_position: String,
    /// Whether a test shows the client labels the text a hook injects as
    /// untrusted data. `false` until one does (026 B-11, D-4).
    pub data_position: bool,
    /// What `data_position` rests on.
    pub data_position_evidence: String,
    /// The test in `tests/clients.rs` that shows it, required when
    /// `data_position` is true and absent otherwise (026 B-11).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_position_test: Option<String>,
}

/// Why a matrix was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MatrixError {
    /// The 1-based line, or 0 for a whole-file problem.
    pub line: usize,
    /// What is wrong.
    pub reason: String,
}

impl std::fmt::Display for MatrixError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "matrix line {}: {}", self.line, self.reason)
    }
}

impl std::error::Error for MatrixError {}

/// The parsed matrix, by client name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Matrix {
    rows: BTreeMap<String, Row>,
}

#[derive(Clone, Debug)]
enum Value {
    Str(String),
    Bool(bool),
    List(Vec<String>),
}

fn err(line: usize, reason: impl Into<String>) -> MatrixError {
    MatrixError {
        line,
        reason: reason.into(),
    }
}

/// `text` without a trailing `#` comment that sits outside a string.
fn strip_comment(text: &str) -> &str {
    let mut in_string = false;
    let mut escaped = false;
    for (index, ch) in text.char_indices() {
        match ch {
            _ if escaped => escaped = false,
            '\\' if in_string => escaped = true,
            '"' => in_string = !in_string,
            '#' if !in_string => return &text[..index],
            _ => {}
        }
    }
    text
}

fn parse_string(text: &str, line: usize) -> Result<(String, &str), MatrixError> {
    let rest = text
        .strip_prefix('"')
        .ok_or_else(|| err(line, "expected a double-quoted string"))?;
    let mut out = String::new();
    let mut chars = rest.char_indices();
    while let Some((index, ch)) = chars.next() {
        match ch {
            '"' => return Ok((out, &rest[index + 1..])),
            '\\' => match chars.next() {
                Some((_, '"')) => out.push('"'),
                Some((_, '\\')) => out.push('\\'),
                Some((_, 'n')) => out.push('\n'),
                _ => return Err(err(line, "only \\\", \\\\, and \\n escapes are read")),
            },
            other => out.push(other),
        }
    }
    Err(err(line, "unterminated string"))
}

fn parse_value(text: &str, line: usize) -> Result<Value, MatrixError> {
    let text = text.trim();
    if text == "true" || text == "false" {
        return Ok(Value::Bool(text == "true"));
    }
    if let Some(inner) = text.strip_prefix('[') {
        let mut rest = inner.trim_start();
        let mut items = Vec::new();
        loop {
            if let Some(after) = rest.strip_prefix(']') {
                return if after.trim().is_empty() {
                    Ok(Value::List(items))
                } else {
                    Err(err(line, "text after the closing bracket"))
                };
            }
            let (item, after) = parse_string(rest, line)?;
            items.push(item);
            rest = after.trim_start();
            if let Some(after) = rest.strip_prefix(',') {
                rest = after.trim_start();
            } else if !rest.starts_with(']') {
                return Err(err(line, "a list holds quoted strings separated by commas"));
            }
        }
    }
    let (value, after) = parse_string(text, line)?;
    if after.trim().is_empty() {
        Ok(Value::Str(value))
    } else {
        Err(err(line, "text after the closing quote"))
    }
}

fn take_str(
    table: &mut BTreeMap<String, Value>,
    key: &str,
    line: usize,
) -> Result<Option<String>, MatrixError> {
    match table.remove(key) {
        None => Ok(None),
        Some(Value::Str(value)) => Ok(Some(value)),
        Some(_) => Err(err(line, format!("`{key}` is a string"))),
    }
}

fn take_bool(
    table: &mut BTreeMap<String, Value>,
    key: &str,
    line: usize,
) -> Result<bool, MatrixError> {
    match table.remove(key) {
        Some(Value::Bool(value)) => Ok(value),
        _ => Err(err(
            line,
            format!("`{key}` is required and is true or false"),
        )),
    }
}

fn take_list(
    table: &mut BTreeMap<String, Value>,
    key: &str,
    line: usize,
) -> Result<Vec<String>, MatrixError> {
    match table.remove(key) {
        Some(Value::List(value)) => Ok(value),
        _ => Err(err(
            line,
            format!("`{key}` is required and is a list of strings"),
        )),
    }
}

fn is_date(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() == 10
        && bytes.iter().enumerate().all(|(index, byte)| {
            if index == 4 || index == 7 {
                *byte == b'-'
            } else {
                byte.is_ascii_digit()
            }
        })
}

fn row(mut table: BTreeMap<String, Value>, line: usize) -> Result<Row, MatrixError> {
    let transport = take_list(&mut table, "transport", line)?;
    let oauth = take_bool(&mut table, "oauth", line)?;
    let dynamic_registration = take_bool(&mut table, "dynamic_registration", line)?;
    let compat_pair = take_bool(&mut table, "compat_pair", line)?;
    let capture =
        take_str(&mut table, "capture", line)?.ok_or_else(|| err(line, "`capture` is required"))?;
    let capture_scopes = take_list(&mut table, "capture_scopes", line)?;
    let verified = take_bool(&mut table, "verified", line)?;
    let min_version = take_str(&mut table, "min_version", line)?;
    let verified_on = take_str(&mut table, "verified_on", line)?;
    let fixtures = take_str(&mut table, "fixtures", line)?
        .ok_or_else(|| err(line, "`fixtures` is required"))?;
    let note = take_str(&mut table, "note", line)?;
    let read_pointer = take_str(&mut table, "read_pointer", line)?.ok_or_else(|| {
        err(
            line,
            "`read_pointer` is required: the client's per-turn hook mechanism, or \"none\" (026 B-11)",
        )
    })?;
    let read_pointer_position = take_str(&mut table, "read_pointer_position", line)?
        .ok_or_else(|| {
            err(
                line,
                "`read_pointer_position` is required: where the client places a hook's output (026 B-11)",
            )
        })?;
    let data_position = take_bool(&mut table, "data_position", line)?;
    let data_position_evidence =
        take_str(&mut table, "data_position_evidence", line)?.ok_or_else(|| {
            err(
                line,
                "`data_position_evidence` is required: what `data_position` rests on (026 B-11)",
            )
        })?;
    let data_position_test = take_str(&mut table, "data_position_test", line)?;
    if let Some(unknown) = table.keys().next() {
        return Err(err(line, format!("unknown key `{unknown}`")));
    }
    if transport.is_empty() {
        return Err(err(line, "`transport` names at least one transport"));
    }
    if capture_scopes.iter().any(|scope| scope == PROMOTE_SCOPE) {
        return Err(err(line, "capture never requests memory.promote (B-6)"));
    }
    // 026 B-11: a data position is a fact a test shows, so a row claims one
    // only by naming that test, and a row that names a test claims it.
    match (data_position, &data_position_test) {
        (true, None) => {
            return Err(err(
                line,
                "a row with `data_position = true` names the test that shows it in `data_position_test` (026 B-11)",
            ));
        }
        (false, Some(_)) => {
            return Err(err(
                line,
                "`data_position_test` is for a row with `data_position = true`; this row says false (026 B-11)",
            ));
        }
        (true, Some(_)) | (false, None) => {}
    }
    // FR-002: a verification date and a fixture set, or an explicit false.
    if verified {
        match &verified_on {
            Some(date) if is_date(date) => {}
            _ => {
                return Err(err(
                    line,
                    "a verified row carries `verified_on` as YYYY-MM-DD",
                ));
            }
        }
        if min_version.is_none() {
            return Err(err(line, "a verified row carries `min_version`"));
        }
    } else if note.is_none() {
        return Err(err(line, "an unverified row says why in `note`"));
    }
    Ok(Row {
        transport,
        oauth,
        dynamic_registration,
        compat_pair,
        capture,
        capture_scopes,
        verified,
        min_version,
        verified_on,
        fixtures,
        note,
        read_pointer,
        read_pointer_position,
        data_position,
        data_position_evidence,
        data_position_test,
    })
}

impl Matrix {
    /// The matrix shipped under `clients/`.
    ///
    /// # Panics
    ///
    /// When the embedded file is malformed, which the crate's tests refuse.
    #[must_use]
    #[allow(clippy::expect_used)]
    pub fn shipped() -> Self {
        Self::parse(MATRIX).expect("clients/matrix.toml is well formed")
    }

    /// Read a matrix.
    ///
    /// # Errors
    ///
    /// The line and reason of the first thing outside the subset or a row
    /// that breaks B-1, B-6, or FR-002.
    pub fn parse(text: &str) -> Result<Self, MatrixError> {
        let mut rows = BTreeMap::new();
        let mut current: Option<(String, usize, BTreeMap<String, Value>)> = None;
        let finish = |current: Option<(String, usize, BTreeMap<String, Value>)>,
                      rows: &mut BTreeMap<String, Row>|
         -> Result<(), MatrixError> {
            if let Some((name, line, table)) = current {
                rows.insert(name, row(table, line)?);
            }
            Ok(())
        };
        for (index, raw) in text.lines().enumerate() {
            let line = index + 1;
            let content = strip_comment(raw).trim();
            if content.is_empty() {
                continue;
            }
            if let Some(header) = content.strip_prefix('[') {
                finish(current.take(), &mut rows)?;
                let name = header
                    .strip_suffix(']')
                    .and_then(|header| header.strip_prefix("client."))
                    .ok_or_else(|| err(line, "a table is `[client.<name>]`"))?;
                let legal = !name.is_empty()
                    && name.bytes().all(|byte| {
                        byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'
                    });
                if !legal {
                    return Err(err(
                        line,
                        "a client name is lowercase ascii, digits, and `-`",
                    ));
                }
                if rows.contains_key(name) {
                    return Err(err(line, format!("`{name}` appears twice")));
                }
                current = Some((name.to_owned(), line, BTreeMap::new()));
                continue;
            }
            let Some((_, _, table)) = current.as_mut() else {
                return Err(err(line, "a key belongs under a `[client.<name>]` table"));
            };
            let (key, value) = content
                .split_once('=')
                .ok_or_else(|| err(line, "expected `key = value`"))?;
            let key = key.trim();
            if table
                .insert(key.to_owned(), parse_value(value, line)?)
                .is_some()
            {
                return Err(err(line, format!("`{key}` is set twice")));
            }
        }
        finish(current.take(), &mut rows)?;
        if rows.is_empty() {
            return Err(err(0, "the matrix has no clients"));
        }
        Ok(Self { rows })
    }

    /// The row for `name`.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Row> {
        self.rows.get(name)
    }

    /// Every client name, in order.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.rows.keys().map(String::as_str)
    }
}

/// The URL a client connects to: the deployment's public URL and the MCP path.
#[must_use]
pub fn mcp_url(public_url: &str) -> String {
    format!("{}{MCP_PATH}", public_url.trim_end_matches('/'))
}

/// `template` with the deployment's URLs filled in (B-3).
#[must_use]
pub fn fill(template: &str, public_url: &str) -> String {
    let public = public_url.trim_end_matches('/');
    template
        .replace("{{mcp_url}}", &mcp_url(public))
        .replace("{{public_url}}", public)
}

/// What `GET /clients/{name}` answers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ClientSnippet {
    /// The client's name.
    pub client: String,
    /// The one URL the manual path adds as a remote MCP server (B-2).
    pub mcp_url: String,
    /// The filled-in snippet, ready to copy.
    pub snippet: String,
    /// The client's matrix row.
    pub matrix: Row,
}

/// The snippet for `name` against `public_url`.
#[must_use]
pub fn snippet_for(matrix: &Matrix, name: &str, public_url: &str) -> Option<ClientSnippet> {
    let row = matrix.get(name)?;
    let body = template(name)?;
    Some(ClientSnippet {
        client: name.to_owned(),
        mcp_url: mcp_url(public_url),
        snippet: fill(body, public_url),
        matrix: row.clone(),
    })
}

/// `GET /clients/{name}` (FR-001).
///
/// Answers for any caller holding `memory.read`; the route lives under the
/// bearer-protected prefix (spec 022 D-4).
///
/// # Errors
///
/// 404 `unknown_client` for a name not in the matrix.
pub async fn get_snippet(
    State(api): State<Api>,
    _caller: Caller,
    Path(name): Path<String>,
) -> ApiResult<Json<ClientSnippet>> {
    let public = api.app.config().public_url.as_str();
    snippet_for(&Matrix::shipped(), &name, public)
        .map(Json)
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "unknown_client",
                "no such client in the matrix",
            )
        })
}
