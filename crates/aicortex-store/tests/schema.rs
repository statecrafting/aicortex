//! The schema, checked (spec 012).
//!
//! - B-1, FR-001: the migrations apply in order from empty and a second run
//!   is a no-op.
//! - B-2: the five tables and the three indexes exist, and the fingerprint
//!   index is unique among the memories that still exist.
//! - B-3, FR-007: no statement in this crate selects from a scoped table
//!   without naming `scope_id`. The check is a function over text, so it is
//!   run once over the crate and once over a synthetic input carrying exactly
//!   the defect it exists to catch.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

mod common;

use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Name {
    name: String,
}

#[derive(Debug, Deserialize)]
struct IndexInfo {
    name: String,
    sql: Option<String>,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr001_the_migrations_apply_in_order_and_a_rerun_applies_nothing() {
    let fixture = common::open().await;
    let store = fixture.handle();

    let first = store.migrate(aicortex_store::migrations()).await.unwrap();
    assert_eq!(first.previous, 0, "an empty store is at the baseline");
    assert_eq!(first.current, aicortex_store::EXPECTED_SCHEMA_VERSION);
    assert_eq!(
        first.applied,
        vec![
            aicortex_store::migrations::COORDINATION_VERSION,
            aicortex_store::migrations::MEMORY_TABLES_VERSION
        ],
        "every migration applied, in version order"
    );

    let second = store.migrate(aicortex_store::migrations()).await.unwrap();
    assert_eq!(second.previous, aicortex_store::EXPECTED_SCHEMA_VERSION);
    assert_eq!(second.current, aicortex_store::EXPECTED_SCHEMA_VERSION);
    assert!(
        second.applied.is_empty(),
        "the second run applied {:?}",
        second.applied
    );

    fixture.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b2_the_tables_and_indexes_are_the_ones_the_spec_names() {
    let fixture = common::Fixture::migrated().await;
    let store = fixture.handle();

    let tables: Vec<Name> = store
        .query_consistent(
            "SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name",
            vec![],
        )
        .await
        .unwrap();
    let names: Vec<&str> = tables.iter().map(|table| table.name.as_str()).collect();
    for expected in [
        "memory",
        "memory_derivation",
        "outbox",
        "provenance",
        "scope",
        "scope_counter",
    ] {
        assert!(
            names.contains(&expected),
            "{expected} is missing: {names:?}"
        );
    }

    let indexes: Vec<IndexInfo> = store
        .query_consistent(
            "SELECT name, sql FROM sqlite_master WHERE type = 'index' ORDER BY name",
            vec![],
        )
        .await
        .unwrap();
    let by_name = |wanted: &str| {
        indexes
            .iter()
            .find(|index| index.name == wanted)
            .unwrap_or_else(|| panic!("{wanted} is missing"))
    };
    by_name("memory_scope_status_created");
    by_name("memory_status_updated");
    let fingerprint = by_name("memory_scope_fingerprint");
    let sql = fingerprint.sql.as_deref().unwrap_or_default();
    assert!(
        sql.contains("UNIQUE"),
        "the fingerprint index is not unique"
    );
    assert!(
        sql.contains("status <> 'erased'"),
        "the fingerprint index is not partial: {sql}"
    );

    fixture.shutdown().await;
}

// FR-007. The tables whose every `SELECT` must name a scope, and the check
// over them.

/// The tables a read of which is a read of somebody's memory.
const SCOPED_TABLES: [&str; 5] = [
    "memory",
    "memory_derivation",
    "provenance",
    "scope",
    "scope_counter",
];

/// The predicate column every such statement must name.
const SCOPE_COLUMN: &str = "scope_id";

/// Every `SELECT` in `source` that reads a scoped table without naming
/// `scope_id` in the same statement (B-3).
///
/// A function over text rather than over the database, because the property
/// is about what the crate can express at all: a statement that does not name
/// the scope is a statement that could return another subject's memory, and
/// finding it needs no store.
fn unscoped_selects(source: &str) -> Vec<String> {
    let mut problems = Vec::new();
    for literal in string_literals(source) {
        for statement in literal.split(';') {
            let lowered = statement.to_lowercase();
            if !lowered.contains("select") {
                continue;
            }
            let reads_scoped_table = SCOPED_TABLES.iter().any(|table| {
                lowered.contains(&format!("from {table}"))
                    || lowered.contains(&format!("join {table}"))
            });
            if reads_scoped_table && !lowered.contains(SCOPE_COLUMN) {
                problems.push(statement.split_whitespace().collect::<Vec<_>>().join(" "));
            }
        }
    }
    problems
}

/// The Rust string literals of `source`, with line comments removed.
///
/// A line comment is dropped only when an even number of unescaped quotes
/// precedes it, so a `//` inside a literal stays part of the literal.
fn string_literals(source: &str) -> Vec<String> {
    let mut code = String::with_capacity(source.len());
    for line in source.lines() {
        let mut quotes = 0usize;
        let mut escaped = false;
        let mut cut = line.len();
        let bytes = line.as_bytes();
        let mut at = 0;
        while at < bytes.len() {
            let byte = bytes[at];
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                quotes += 1;
            } else if byte == b'/'
                && at + 1 < bytes.len()
                && bytes[at + 1] == b'/'
                && quotes.is_multiple_of(2)
            {
                cut = at;
                break;
            }
            at += 1;
        }
        code.push_str(&line[..cut]);
        code.push('\n');
    }

    let mut literals = Vec::new();
    let mut current: Option<String> = None;
    let mut escaped = false;
    for character in code.chars() {
        match current.as_mut() {
            None => {
                if character == '"' {
                    current = Some(String::new());
                }
            }
            Some(literal) => {
                if escaped {
                    literal.push(character);
                    escaped = false;
                } else if character == '\\' {
                    escaped = true;
                } else if character == '"' {
                    literals.push(current.take().unwrap_or_default());
                } else {
                    literal.push(character);
                }
            }
        }
    }
    literals
}

/// Every `.rs` file of this crate's `src`.
fn crate_sources() -> Vec<(String, String)> {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut pending = vec![src.clone()];
    let mut sources = Vec::new();
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).expect("the crate's src is readable") {
            let path: PathBuf = entry.expect("a directory entry").path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                let name = path
                    .strip_prefix(&src)
                    .expect("a path under src")
                    .to_string_lossy()
                    .into_owned();
                sources.push((name, std::fs::read_to_string(&path).expect("a source file")));
            }
        }
    }
    sources.sort();
    sources
}

#[test]
fn b3_fr007_no_statement_selects_a_scoped_table_without_its_scope() {
    let sources = crate_sources();
    assert!(
        sources.iter().any(|(name, _)| name == "memory_repo.rs"),
        "the scan found no sources, so it proved nothing"
    );
    let problems: Vec<String> = sources
        .iter()
        .flat_map(|(name, source)| {
            unscoped_selects(source)
                .into_iter()
                .map(move |statement| format!("{name}: {statement}"))
        })
        .collect();
    assert!(problems.is_empty(), "B-3 does not hold: {problems:#?}");
}

#[test]
fn fr007_the_check_refuses_an_unscoped_select() {
    let scoped = r#"const OK: &str = "SELECT record FROM memory WHERE scope_id = $1 AND id = $2";"#;
    assert!(
        unscoped_selects(scoped).is_empty(),
        "a scoped read was refused"
    );

    for defect in [
        r#"const BAD: &str = "SELECT record FROM memory WHERE id = $1";"#,
        r#"const BAD: &str = "SELECT count FROM scope_counter";"#,
        r#"const BAD: &str = "SELECT parent_id FROM memory_derivation WHERE memory_id = $1";"#,
        r#"const BAD: &str = "INSERT INTO x SELECT 1 FROM provenance WHERE memory_id = $1";"#,
    ] {
        assert!(
            !unscoped_selects(defect).is_empty(),
            "the check accepted {defect}"
        );
    }

    // A comment is not code, and a literal that merely mentions `//` is.
    let commented = r#"// SELECT record FROM memory WHERE id = $1"#;
    assert!(
        unscoped_selects(commented).is_empty(),
        "a comment was read as code"
    );
    let with_slashes = r#"const OK: &str = "SELECT record FROM memory WHERE scope_id = $1 AND locator = 'https://x'";"#;
    assert!(
        unscoped_selects(with_slashes).is_empty(),
        "a literal carrying // was cut short"
    );
}
