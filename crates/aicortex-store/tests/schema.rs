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
    let declared: Vec<u32> = aicortex_store::migrations()
        .iter()
        .map(|migration| migration.version)
        .collect();
    assert_eq!(
        first.applied, declared,
        "every migration applied, in version order"
    );
    assert!(
        declared.windows(2).all(|pair| pair[0] < pair[1]),
        "the migration list is not strictly ascending: {declared:?}"
    );
    assert!(
        declared.len() >= 3,
        "the scan found {} migrations, so it proved nothing",
        declared.len()
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
        "decision_key",
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
    by_name("decision_key_scope_memory");
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
///
/// `decision_key` is one of them (spec 013 B-9): a key is an object of
/// somebody's scope, and a read that could reach across scopes would be a way
/// to confirm another subject's refusal digests.
const SCOPED_TABLES: [&str; 6] = [
    "memory",
    "memory_derivation",
    "provenance",
    "scope",
    "scope_counter",
    "decision_key",
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

/// The Rust string literals of `source`, with comments skipped.
///
/// One state machine over the whole file rather than a heuristic per line: a
/// SQL constant may span lines, and a scanner that reset its notion of "am I
/// inside a string" at every newline would cut a continuation line short at a
/// `//` that is part of the statement. A guard that silently reads less than
/// it thinks it does is worse than no guard, so this tracks the three states
/// it needs and nothing else.
///
/// It reads plain `"..."` literals, which is every literal this crate holds
/// (asserted below). A block comment carrying a quote would open a phantom
/// literal, which can only add a false positive and so fails loudly rather
/// than quietly, which is the right direction for a guard to be wrong in.
fn string_literals(source: &str) -> Vec<String> {
    enum State {
        Code,
        Literal(String),
        Comment,
    }

    let mut state = State::Code;
    let mut escaped = false;
    let mut literals = Vec::new();
    let mut characters = source.chars().peekable();
    while let Some(character) = characters.next() {
        match &mut state {
            State::Code => {
                if character == '"' {
                    state = State::Literal(String::new());
                } else if character == '/' && characters.peek() == Some(&'/') {
                    characters.next();
                    state = State::Comment;
                }
            }
            State::Literal(literal) => {
                if escaped {
                    literal.push(character);
                    escaped = false;
                } else if character == '\\' {
                    escaped = true;
                } else if character == '"' {
                    if let State::Literal(finished) = std::mem::replace(&mut state, State::Code) {
                        literals.push(finished);
                    }
                } else {
                    literal.push(character);
                }
            }
            State::Comment => {
                if character == '\n' {
                    state = State::Code;
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

    // An empty violations list is only evidence if the scanner actually read
    // the crate's statements. These two assertions are what stand between a
    // pass and a scanner that silently returned nothing: the `SELECT`s this
    // crate holds are found, and every literal it holds is of the one shape
    // `string_literals` reads.
    let reads: Vec<String> = sources
        .iter()
        .flat_map(|(_, source)| string_literals(source))
        .filter(|literal| literal.to_lowercase().contains("select"))
        .collect();
    assert!(
        reads.len() >= 5,
        "the scanner found only {} SELECT statements in the crate: {reads:#?}",
        reads.len()
    );
    assert!(
        reads
            .iter()
            .any(|literal| literal.contains("FROM memory WHERE scope_id")),
        "the scanner did not read the memory detail statement: {reads:#?}"
    );
    for (name, source) in &sources {
        for hazard in ["r#\"", "/*", "'\"'"] {
            assert!(
                !source.contains(hazard),
                "{name} holds {hazard}, which `string_literals` does not read"
            );
        }
    }

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
    let doc_commented = r#"/// SELECT record FROM memory WHERE id = $1"#;
    assert!(
        unscoped_selects(doc_commented).is_empty(),
        "a doc comment was read as code"
    );

    // The case a per-line scanner gets wrong: a literal spanning lines whose
    // continuation carries `//`. The statement must be read whole, so its
    // scope predicate on the first line still counts for the second.
    let multiline = "const OK: &str = \"SELECT record FROM memory\n    WHERE scope_id = $1 AND locator = 'https://x'\";";
    assert!(
        unscoped_selects(multiline).is_empty(),
        "a multi-line literal was cut at its //"
    );
    let multiline_bad =
        "const BAD: &str = \"SELECT record FROM memory\n    WHERE locator = 'https://x'\";";
    assert!(
        !unscoped_selects(multiline_bad).is_empty(),
        "a multi-line unscoped statement slipped through"
    );
    let with_slashes = r#"const OK: &str = "SELECT record FROM memory WHERE scope_id = $1 AND locator = 'https://x'";"#;
    assert!(
        unscoped_selects(with_slashes).is_empty(),
        "a literal carrying // was cut short"
    );
}
