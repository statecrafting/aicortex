//! The client adapters (spec 022 B-1, B-3, B-4, B-7, B-8, FR-001 to FR-006).
//!
//! The hooks are the shipped scripts under `clients/`, run with Node against
//! each client's fixtures; the server they talk to is either a stub that
//! records what it is sent or the real cell. Node must be installed: a
//! missing interpreter fails the test rather than skipping it, because a
//! skipped hook test would make the claims in the matrix unbacked.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use aicortex_api::clients::{self, Matrix};
use aicortex_api::dto::CaptureRequest;
use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::post;
use common::Who;
use reqwest::Method;
use serde_json::Value;

const CLIENTS_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../clients");

/// The five durable kinds the fixtures hold, in the order they appear.
const DURABLE: [&str; 5] = ["preference", "correction", "decision", "task", "fact"];

fn root() -> PathBuf {
    Path::new(CLIENTS_DIR)
        .canonicalize()
        .expect("the clients tree")
}

fn names() -> Vec<String> {
    Matrix::shipped().names().map(str::to_owned).collect()
}

/// Run one hook invocation of `client`'s fixture set against `url`.
fn run_hook(client: &str, invocation: &Value, url: &str, extra: &[(&str, &str)], scratch: &Path) {
    let dir = root().join(client);
    let fixtures = dir.join("fixtures");
    let fill = |value: &Value| {
        value
            .to_string()
            .replace("${FIXTURES}", fixtures.to_str().expect("a utf-8 path"))
    };
    let mut command = Command::new("node");
    command
        .arg(dir.join("hook.mjs"))
        .current_dir(scratch)
        // Only what this test sets reaches the hook: nothing inherited from
        // the runner's environment can supply a token or turn capture off.
        .env_remove("AICORTEX_ACCESS_TOKEN")
        .env_remove("AICORTEX_TOKEN_FILE")
        .env_remove("AICORTEX_CAPTURE")
        .env_remove("AICORTEX_HOOK_TIMEOUT_MS")
        .env_remove("AICORTEX_TRANSCRIPT_ROOT")
        .env_remove("CLAUDE_CONFIG_DIR")
        .env("AICORTEX_URL", url)
        .env("AICORTEX_HOME", scratch)
        .env("AICORTEX_HOOK_LOG", scratch.join("hook.log"))
        .env("AICORTEX_TRANSCRIPT_ROOT", &fixtures)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    for (key, value) in extra {
        command.env(key, value);
    }
    if let Some(args) = invocation.get("args").and_then(Value::as_array) {
        for arg in args {
            command.arg(match arg {
                Value::String(text) => text.clone(),
                other => fill(other),
            });
        }
    }
    let mut child = command
        .spawn()
        .expect("node is installed and runs the hook");
    {
        use std::io::Write as _;
        let mut stdin = child.stdin.take().expect("a stdin pipe");
        if let Some(input) = invocation.get("stdin") {
            stdin
                .write_all(fill(input).as_bytes())
                .expect("stdin writes");
        }
    }
    let output = child.wait_with_output().expect("the hook ends");
    assert!(
        output.status.success(),
        "{client}: a hook never fails its client (B-4): {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn invocations(client: &str) -> Vec<Value> {
    let text = std::fs::read_to_string(root().join(client).join("fixtures/invocations.json"))
        .expect("the fixture set");
    serde_json::from_str::<Vec<Value>>(&text).expect("the fixture set is a JSON list")
}

type Seen = Arc<Mutex<Vec<(Option<String>, String)>>>;

/// A server that records each capture request and answers `status`.
async fn stub(status: StatusCode) -> (String, Seen) {
    let seen: Seen = Arc::default();
    let app =
        Router::new()
            .route(
                "/api/v1/capture",
                post(
                    move |State(seen): State<Seen>,
                          headers: axum::http::HeaderMap,
                          body: String| async move {
                        let auth = headers
                            .get("authorization")
                            .and_then(|value| value.to_str().ok())
                            .map(str::to_owned);
                        seen.lock().unwrap().push((auth, body));
                        status
                    },
                ),
            )
            .with_state(seen.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    (url, seen)
}

/// Run every invocation of `client` off the async threads.
async fn run_all(client: &str, url: &str, extra: &[(&str, &str)], scratch: &Path) {
    let client = client.to_owned();
    let url = url.to_owned();
    let extra: Vec<(String, String)> = extra
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect();
    let scratch = scratch.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let extra: Vec<(&str, &str)> = extra
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        for invocation in invocations(&client) {
            run_hook(&client, &invocation, &url, &extra, &scratch);
        }
    })
    .await
    .expect("the hooks ran");
}

/// B-1, B-7, FR-002: every row is complete, backed by a directory, a snippet,
/// a hook, a README, and a fixture set, and nothing exists without a row.
#[test]
fn the_matrix_and_the_tree_agree() {
    let matrix = Matrix::shipped();
    for name in names() {
        let row = matrix.get(&name).unwrap();
        let dir = root().join(&name);
        for file in ["README.md", "snippet.txt", "hook.mjs"] {
            assert!(dir.join(file).is_file(), "{name} has no {file}");
        }
        assert!(
            clients::template(&name).is_some(),
            "{name} has no embedded template"
        );
        assert!(
            root()
                .join(&row.fixtures)
                .join("invocations.json")
                .is_file(),
            "{name} names no fixture set"
        );
        assert!(
            row.verified || row.note.is_some(),
            "{name}: neither verified nor said to be unverified"
        );
        assert!(!row.capture_scopes.contains(&"memory.promote".to_owned()));
        assert!(!row.compat_pair || row.note.is_some());
    }
    let mut on_disk: Vec<String> = std::fs::read_dir(root())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_dir())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    on_disk.sort();
    assert_eq!(
        on_disk,
        names(),
        "a client directory with no matrix row, or the reverse"
    );
}

#[test]
fn a_matrix_outside_the_rules_is_refused() {
    let row = |extra: &str| {
        format!(
            "[client.x]\ntransport = [\"streamable-http\"]\noauth = true\ndynamic_registration = true\n\
             compat_pair = false\ncapture = \"hook\"\ncapture_scopes = [\"memory.write\"]\n\
             fixtures = \"x/fixtures\"\nread_pointer = \"none\"\n\
             read_pointer_position = \"not applicable\"\ndata_position = false\n\
             data_position_evidence = \"none\"\n{extra}"
        )
    };
    assert!(Matrix::parse(&row("verified = false\nnote = \"why\"\n")).is_ok());
    for (bad, why) in [
        ("verified = false\n", "unverified without a note"),
        (
            "verified = true\nmin_version = \"1\"\n",
            "verified without a date",
        ),
        (
            "verified = true\nverified_on = \"2026-11-26\"\n",
            "verified without a version",
        ),
        (
            "verified = false\nnote = \"n\"\nsurprise = true\n",
            "unknown key",
        ),
    ] {
        assert!(Matrix::parse(&row(bad)).is_err(), "{why}");
    }
    let promote = row("verified = false\nnote = \"n\"\n")
        .replace("\"memory.write\"", "\"memory.write\", \"memory.promote\"");
    assert!(
        Matrix::parse(&promote).is_err(),
        "capture never asks to promote (B-6)"
    );
    assert!(Matrix::parse("").is_err());
    let verified = row("verified = true\nverified_on = \"2026-11-26\"\nmin_version = \"1.0\"\n");
    assert!(Matrix::parse(&verified).is_ok());
}

#[test]
fn the_url_is_filled_in_and_no_placeholder_survives() {
    for name in names() {
        for public in ["https://mem.example.org", "https://mem.example.org/"] {
            let served = clients::snippet_for(&Matrix::shipped(), &name, public).unwrap();
            assert_eq!(served.mcp_url, "https://mem.example.org/mcp");
            assert!(
                served.snippet.contains("https://mem.example.org/mcp"),
                "{name}"
            );
            assert!(!served.snippet.contains("{{"), "{name} keeps a placeholder");
            assert!(!served.snippet.contains("org//"), "{name}: doubled slash");
        }
    }
}

/// FR-001: through the chassis, the snippet carries the deployment's public URL.
#[tokio::test(flavor = "multi_thread")]
async fn fr001_the_route_serves_each_clients_snippet() {
    let cell = common::boot().await;
    let who = Who::person("sub-clients", &["memory.read"]);
    for name in names() {
        let response = cell
            .request(Method::GET, &format!("/api/v1/clients/{name}"), Some(&who))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200, "{name}");
        let body: Value = response.json().await.unwrap();
        assert_eq!(body["client"], name.as_str());
        assert_eq!(body["mcp_url"], format!("{}/mcp", cell.origin));
        assert!(
            body["snippet"]
                .as_str()
                .unwrap()
                .contains(&format!("{}/mcp", cell.origin))
        );
        assert_eq!(body["matrix"]["verified"], false);
    }
    let unknown = cell
        .request(Method::GET, "/api/v1/clients/nonesuch", Some(&who))
        .send()
        .await
        .unwrap();
    assert_eq!(unknown.status(), 404);
    let anonymous = cell
        .request(Method::GET, "/api/v1/clients/codex", None)
        .send()
        .await
        .unwrap();
    assert_eq!(anonymous.status(), 401);
    cell.shutdown().await;
}

/// FR-003 and FR-005: each client's recorded session produces exactly five
/// payloads, each valid against the capture DTO, one per durable kind, and
/// none for a runtime item.
#[tokio::test(flavor = "multi_thread")]
async fn fr003_fr005_a_session_yields_the_five_durable_items_only() {
    for client in names() {
        let (url, seen) = stub(StatusCode::OK).await;
        let scratch = tempfile::tempdir().unwrap();
        run_all(
            &client,
            &url,
            &[("AICORTEX_ACCESS_TOKEN", "t")],
            scratch.path(),
        )
        .await;
        let seen = seen.lock().unwrap().clone();
        let mut kinds = Vec::new();
        for (auth, body) in &seen {
            assert_eq!(auth.as_deref(), Some("Bearer t"), "{client}");
            let request: CaptureRequest = serde_json::from_str(body)
                .unwrap_or_else(|error| panic!("{client}: not a capture request: {error}: {body}"));
            for candidate in &request.candidates {
                let source = candidate.source.as_ref().expect("a source");
                assert_eq!(source.system.as_deref(), Some(client.as_str()), "B-4");
                assert!(
                    source
                        .external_id
                        .as_deref()
                        .unwrap()
                        .starts_with("fixture-")
                );
                assert!(!candidate.text.contains('\n'));
                kinds.push(candidate.kind.label());
            }
        }
        assert_eq!(
            kinds, DURABLE,
            "{client}: exactly the durable items, in order"
        );
        let text = seen
            .iter()
            .map(|(_, body)| body.as_str())
            .collect::<String>();
        for runtime in [
            "Working on",
            "printed by a tool",
            "/Users/",
            "mcpServers",
            "EXAMPLE-NOT-A-REAL",
            "plugin",
            "another type",
            "system event",
            "caveat injected",
        ] {
            assert!(
                !text.contains(runtime),
                "{client}: sent runtime state {runtime:?}"
            );
        }
    }
}

/// FR-004: a server answering 503, or none at all, never makes the hook fail,
/// and the failure is recorded locally.
#[tokio::test(flavor = "multi_thread")]
async fn fr004_a_failing_server_is_silent_to_the_client_and_logged() {
    for client in names() {
        let (url, seen) = stub(StatusCode::SERVICE_UNAVAILABLE).await;
        let scratch = tempfile::tempdir().unwrap();
        run_all(
            &client,
            &url,
            &[("AICORTEX_ACCESS_TOKEN", "t")],
            scratch.path(),
        )
        .await;
        assert!(!seen.lock().unwrap().is_empty(), "{client}: the hook tried");
        let log = std::fs::read_to_string(scratch.path().join("hook.log")).unwrap();
        assert!(log.contains("HTTP 503"), "{client}: {log}");

        // Nothing listening at all.
        let dead = tempfile::tempdir().unwrap();
        run_all(
            &client,
            "http://127.0.0.1:9",
            &[("AICORTEX_ACCESS_TOKEN", "t")],
            dead.path(),
        )
        .await;
        let log = std::fs::read_to_string(dead.path().join("hook.log")).unwrap();
        assert!(log.contains("capture failed"), "{client}: {log}");
    }
}

/// B-8: the off switch sends nothing; with no token nothing is sent either.
#[tokio::test(flavor = "multi_thread")]
async fn the_off_switch_and_a_missing_token_send_nothing() {
    for client in names() {
        let (url, seen) = stub(StatusCode::OK).await;
        let off = tempfile::tempdir().unwrap();
        run_all(
            &client,
            &url,
            &[("AICORTEX_ACCESS_TOKEN", "t"), ("AICORTEX_CAPTURE", "off")],
            off.path(),
        )
        .await;
        let none = tempfile::tempdir().unwrap();
        run_all(&client, &url, &[], none.path()).await;
        assert!(seen.lock().unwrap().is_empty(), "{client}");
        let log = std::fs::read_to_string(none.path().join("hook.log")).unwrap();
        assert!(log.contains("no access token"), "{client}: {log}");
    }
}

/// The Claude Code hook reads a transcript only from under its projects
/// directory: an input naming any other file reads nothing and sends nothing.
#[tokio::test(flavor = "multi_thread")]
async fn a_transcript_outside_the_projects_directory_is_not_read() {
    let (url, seen) = stub(StatusCode::OK).await;
    let scratch = tempfile::tempdir().unwrap();
    let outside = scratch.path().join("elsewhere.jsonl");
    std::fs::copy(
        root().join("claude-code/fixtures/transcript.jsonl"),
        &outside,
    )
    .unwrap();
    let invocation = serde_json::json!({
        "stdin": { "session_id": "s-outside", "transcript_path": outside },
    });
    run_hook(
        "claude-code",
        &invocation,
        &url,
        &[("AICORTEX_ACCESS_TOKEN", "t")],
        scratch.path(),
    );
    assert!(seen.lock().unwrap().is_empty());
    let log = std::fs::read_to_string(scratch.path().join("hook.log")).unwrap();
    assert!(log.contains("nothing read"), "{log}");
}

/// B-4, B-5, B-6: the real hook against the real cell, with a token that
/// holds read and write only, lands the five items attributed to the client.
#[tokio::test(flavor = "multi_thread")]
async fn the_hook_captures_into_the_real_cell() {
    let cell = common::boot().await;
    let who = Who {
        sub: "sub-hook".to_owned(),
        client: "claude-code".to_owned(),
        scopes: vec!["memory.read", "memory.write"],
    };
    let token = cell.token(&who);
    let scratch = tempfile::tempdir().unwrap();
    run_all(
        "claude-code",
        &cell.origin,
        &[("AICORTEX_ACCESS_TOKEN", &token)],
        scratch.path(),
    )
    .await;
    let response = cell
        .request(Method::GET, "/api/v1/memories", Some(&who))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body: Value = response.json().await.unwrap();
    let mut kinds: Vec<String> = body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["kind"].as_str().unwrap().to_owned())
        .collect();
    kinds.sort();
    let mut want: Vec<String> = DURABLE.iter().map(|kind| (*kind).to_owned()).collect();
    want.sort();
    assert_eq!(kinds, want, "the hook's items landed");

    // Running the same session again merges rather than duplicating.
    run_all(
        "claude-code",
        &cell.origin,
        &[("AICORTEX_ACCESS_TOKEN", &token)],
        scratch.path(),
    )
    .await;
    let again: Value = cell
        .request(Method::GET, "/api/v1/memories", Some(&who))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(again["items"].as_array().unwrap().len(), 5);
    assert!(
        !std::fs::read_to_string(scratch.path().join("hook.log")).is_ok_and(|log| !log.is_empty())
    );
    cell.shutdown().await;
}

/// FR-006: each README states the B-8 rule and names the off switch.
#[test]
fn fr006_every_readme_states_the_rule_and_the_off_switch() {
    for name in names() {
        let readme = std::fs::read_to_string(root().join(&name).join("README.md")).unwrap();
        for needle in [
            "durable items, never runtime state",
            "work in progress, tool output, local file",
            "credentials",
            "plugins",
            "AICORTEX_CAPTURE=off",
        ] {
            assert!(
                readme.replace('\n', " ").contains(needle),
                "{name}'s README lacks {needle:?}"
            );
        }
    }
}

// ---- spec 026 B-11, B-12: the read-pointer capability rows ----

/// Whether `source` holds a test function `name` whose body asserts that the
/// client labels injected text untrusted (026 FR-008): the function exists,
/// carries `#[test]` or a `tokio::test` attribute, and its body names the
/// word `untrusted`.
/// The text of the function that `source` starts with, from its header to
/// its matching closing brace. Braces are counted only in code: a brace in a
/// string literal (a format placeholder such as `"{extra}"`), a character
/// literal, or a line or block comment does not count, and a nested block, closure,
/// or inner scope does not end the body early. Raw strings are not used in
/// the tests this reads.
fn function_body(source: &str) -> &str {
    let Some(open) = source.find('{') else {
        return "";
    };
    let bytes = source.as_bytes();
    let mut depth = 0_usize;
    let mut at = open;
    while at < bytes.len() {
        match bytes[at] {
            b'"' => {
                at += 1;
                while at < bytes.len() && bytes[at] != b'"' {
                    at += if bytes[at] == b'\\' { 2 } else { 1 };
                }
            }
            // An escaped character literal (`'\''`, `'\n'`) first, then a
            // plain one (`'}'`); a lifetime (`'a`) matches neither.
            b'\'' if bytes.get(at + 1) == Some(&b'\\') && bytes.get(at + 3) == Some(&b'\'') => {
                at += 3;
            }
            b'\'' if bytes.get(at + 2) == Some(&b'\'') => at += 2,
            b'/' if bytes.get(at + 1) == Some(&b'/') => {
                while at < bytes.len() && bytes[at] != b'\n' {
                    at += 1;
                }
            }
            b'/' if bytes.get(at + 1) == Some(&b'*') => {
                at += 2;
                while at + 1 < bytes.len() && !(bytes[at] == b'*' && bytes[at + 1] == b'/') {
                    at += 1;
                }
                at += 1;
            }
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return &source[..=at];
                }
            }
            _ => {}
        }
        at += 1;
    }
    source
}

fn is_a_test_asserting_untrusted(source: &str, name: &str) -> bool {
    let header = format!("fn {name}(");
    let Some(at) = source.find(&header) else {
        return false;
    };
    let before = &source[..at];
    let attribute = before.lines().rev().take(3).any(|line| {
        let line = line.trim();
        line.starts_with("#[test") || line.starts_with("#[tokio::test")
    });
    let body = function_body(&source[at..]);
    // An assertion, not a comment or a stray string, must name the label: a
    // line of the body that is code, starts an `assert` macro, and mentions
    // the untrusted marking (026 B-11, 019).
    let asserts_the_label = body.lines().map(str::trim).any(|line| {
        // Only the code before a trailing comment counts.
        let code = line.split("//").next().unwrap_or_default();
        code.starts_with("assert") && code.to_lowercase().contains("untrusted")
    });
    attribute && asserts_the_label
}

/// 026 B-11: a row claims a data position only by naming the test that shows
/// it, and every row says what its hook mechanism and position are.
#[test]
fn b11_the_reader_refuses_a_data_position_without_its_test() {
    let row = |extra: &str| {
        format!(
            "[client.x]\ntransport = [\"streamable-http\"]\noauth = true\ndynamic_registration = true\n\
             compat_pair = false\ncapture = \"hook\"\ncapture_scopes = [\"memory.write\"]\n\
             fixtures = \"x/fixtures\"\nverified = false\nnote = \"why\"\n{extra}"
        )
    };
    let entry = "read_pointer = \"hook\"\nread_pointer_position = \"context\"\n\
                 data_position_evidence = \"a test\"\n";
    assert!(Matrix::parse(&row(&format!("{entry}data_position = false\n"))).is_ok());
    assert!(
        Matrix::parse(&row(&format!(
            "{entry}data_position = true\ndata_position_test = \"a_test\"\n"
        )))
        .is_ok()
    );
    for (bad, why) in [
        (String::new(), "no read_pointer entry at all"),
        (
            "read_pointer_position = \"p\"\ndata_position = false\ndata_position_evidence = \"e\"\n"
                .to_owned(),
            "no read_pointer",
        ),
        (
            "read_pointer = \"h\"\ndata_position = false\ndata_position_evidence = \"e\"\n"
                .to_owned(),
            "no read_pointer_position",
        ),
        (
            "read_pointer = \"h\"\nread_pointer_position = \"p\"\ndata_position_evidence = \"e\"\n"
                .to_owned(),
            "no data_position",
        ),
        (
            "read_pointer = \"h\"\nread_pointer_position = \"p\"\ndata_position = false\n"
                .to_owned(),
            "no data_position_evidence",
        ),
        (
            format!("{entry}data_position = true\n"),
            "a data position with no test named",
        ),
        (
            format!("{entry}data_position = false\ndata_position_test = \"a_test\"\n"),
            "a test named by a row that is not a data position",
        ),
    ] {
        assert!(Matrix::parse(&row(&bad)).is_err(), "{why}");
    }
}

/// 026 FR-008: the check that a named test exists and asserts the client
/// labels injected text untrusted does what it says.
#[test]
fn fr008_the_untrusted_test_check_reads_the_test_source() {
    let source = "\
#[tokio::test]
async fn labels_it() {
    assert!(output.contains(\"untrusted\"));
}

#[test]
fn says_nothing() {
    assert!(true);
}

fn not_a_test() {
    // untrusted
}

#[test]
fn asserts_after_a_nested_block() {
    for line in output.lines() {
        if line.is_empty() {
            continue;
        }
    }
    assert!(output.contains(\"untrusted\"));
}

#[test]
fn formats_before_it_asserts() {
    let shown = format!(\"{extra} }} {{\", extra = '}');
    assert!(shown.contains(\"untrusted\"));
}

#[test]
fn quotes_a_quote_then_asserts() {
    let marks = ['\\'', '}'];
    assert!(output.contains(\"untrusted\"), \"{marks:?}\");
}

#[test]
fn comments_the_label() {
    assert!(output.is_empty()); // untrusted
}

#[test]
fn comments_a_brace_then_asserts() {
    /* a stray } in a block comment */
    assert!(output.contains(\"untrusted\"));
}

#[test]
fn only_says_it() {
    // the client labels this untrusted
    let label = \"untrusted\";
    assert!(!label.is_empty());
}
";
    assert!(is_a_test_asserting_untrusted(source, "labels_it"));
    assert!(
        is_a_test_asserting_untrusted(source, "asserts_after_a_nested_block"),
        "a nested block does not end the body early"
    );
    assert!(
        is_a_test_asserting_untrusted(source, "formats_before_it_asserts"),
        "braces in string and character literals are not counted"
    );
    assert!(
        is_a_test_asserting_untrusted(source, "quotes_a_quote_then_asserts"),
        "an escaped quote literal does not hide a brace"
    );
    assert!(
        is_a_test_asserting_untrusted(source, "comments_a_brace_then_asserts"),
        "a brace in a block comment is not counted"
    );
    assert!(
        !is_a_test_asserting_untrusted(source, "comments_the_label"),
        "a trailing comment is not an assertion"
    );
    assert!(!is_a_test_asserting_untrusted(source, "says_nothing"));
    assert!(!is_a_test_asserting_untrusted(source, "not_a_test"));
    assert!(!is_a_test_asserting_untrusted(source, "absent"));
    assert!(
        !is_a_test_asserting_untrusted(source, "only_says_it"),
        "a comment or a string outside an assertion does not count"
    );
}

/// 026 FR-008: every row has a `read_pointer` entry; a row whose
/// `data_position` is true names a test in this file that exists and asserts
/// the client labels injected text untrusted, and every other row is false;
/// the Claude Code row is false and says why; and no pointer hook is shipped
/// or generated for any client (B-12).
#[test]
fn fr008_every_row_has_a_read_pointer_entry_and_no_hook_carries_pointers() {
    let source =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/clients.rs"))
            .unwrap();
    let matrix = Matrix::shipped();
    for name in names() {
        let row = matrix.get(&name).unwrap();
        assert!(!row.read_pointer.trim().is_empty(), "{name}: read_pointer");
        assert!(
            !row.read_pointer_position.trim().is_empty(),
            "{name}: read_pointer_position"
        );
        assert!(
            !row.data_position_evidence.trim().is_empty(),
            "{name}: data_position_evidence"
        );
        match (&row.data_position, &row.data_position_test) {
            (true, Some(test)) => assert!(
                is_a_test_asserting_untrusted(&source, test),
                "{name} claims a data position but {test} is not a test here that asserts \
                 the client labels injected text untrusted"
            ),
            (true, None) => panic!("{name} claims a data position and names no test"),
            (false, None) => {}
            (false, Some(test)) => panic!("{name} says false and names {test}"),
        }

        // B-12: nothing generated or shipped for a client carries pointers.
        let dir = root().join(&name);
        for file in ["snippet.txt", "hook.mjs"] {
            let text = std::fs::read_to_string(dir.join(file))
                .unwrap()
                .to_lowercase();
            assert!(!text.contains("pointer"), "{name}/{file} mentions pointers");
        }
        for entry in std::fs::read_dir(&dir).unwrap().filter_map(Result::ok) {
            let file = entry.file_name().to_string_lossy().to_lowercase();
            assert!(!file.contains("pointer"), "{name} ships {file}");
        }
        let served = clients::snippet_for(&matrix, &name, "https://mem.example.org").unwrap();
        assert!(!served.snippet.to_lowercase().contains("pointer"), "{name}");
    }
    let core = std::fs::read_to_string(root().join("capture-core.mjs"))
        .unwrap()
        .to_lowercase();
    assert!(
        !core.contains("pointer"),
        "the shared hook core carries no pointers"
    );

    let claude = matrix.get("claude-code").unwrap();
    assert!(!claude.data_position);
    assert!(
        claude
            .read_pointer_position
            .contains("feedback from the user")
    );
    assert!(claude.read_pointer_position.contains("directive"));
}
