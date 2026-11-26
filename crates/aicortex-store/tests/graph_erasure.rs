//! Erasure reaches what the graph derived (spec 017 FR-006, 014 B-7).
//!
//! Graph rows are written directly, as the extraction worker of
//! `aicortex-graph` would, so this crate proves its own half of the
//! obligation: the mentions of an erased memory go, its edges lose it as a
//! source, an edge with no source left is marked `origin_erased` and kept, and
//! a scope erasure removes the whole graph.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

mod common;

use aicortex_store::{Authority, Eraser, Erasure, Lifecycle, ScopeId};
use aicortex_types::{Memory, MemoryId, Scope};
use rahi_store::{Statement, TxnBuilder, Value};
use rahi_types::UnixSeconds;

async fn capture(node: &common::Node, memory: &Memory) -> MemoryId {
    let admitted = common::admit(memory);
    let work = common::work(memory);
    let mut txn = TxnBuilder::new();
    let captured = Lifecycle::new()
        .capture(&node.handle(), &mut txn, &admitted, &work)
        .await
        .expect("the capture stages");
    node.handle()
        .txn(txn.into_statements())
        .await
        .expect("the capture commits");
    captured.id()
}

async fn seed_graph(node: &common::Node, scope: &Scope, a: MemoryId, b: MemoryId, tag: &str) {
    let s = ScopeId::of(scope);
    let sql = |text: &str, params: Vec<Value>| Statement::with_params(text.to_owned(), params);
    let id = |text: &str| Value::from(text);
    let mut statements = vec![
        sql(
            "INSERT INTO entity (entity_id, scope_id, kind, canonical, first_seen, last_seen)
             VALUES (?2, ?1, 'person', 'Dana', 1, 1)",
            vec![Value::from(&s), Value::from(format!("ent{tag}"))],
        ),
        sql(
            "INSERT INTO entity_alias (scope_id, entity_id, alias, normalized)
             VALUES (?1, ?2, 'Dana', 'dana')",
            vec![Value::from(&s), Value::from(format!("ent{tag}"))],
        ),
    ];
    for (memory, start) in [(a, 0), (b, 0)] {
        statements.push(sql(
            "INSERT INTO mention
             (scope_id, memory_id, entity_id, byte_start, byte_end, confidence, extractor,
              extractor_version)
             VALUES (?1, ?2, ?4, ?3, ?3 + 4, 800, 'pattern', 1)",
            vec![
                Value::from(&s),
                Value::from(memory.to_string()),
                Value::Integer(start),
                Value::from(format!("ent{tag}")),
            ],
        ));
        statements.push(sql(
            "INSERT INTO extraction (scope_id, memory_id, extractor, extractor_version, at)
             VALUES (?1, ?2, 'pattern', 1, 1)",
            vec![Value::from(&s), Value::from(memory.to_string())],
        ));
    }
    // `shared` is supported by both memories, `solo` only by the first.
    for (edge, object) in [("shared", "Acme"), ("solo", "Globex")] {
        statements.push(sql(
            "INSERT INTO edge
             (edge_id, scope_id, subject, predicate, object_literal, confidence, extractor,
              extractor_version, valid_from, status, created)
             VALUES (?2, ?1, ?4, 'works_at', ?3, 800, 'pattern', 1, 1, 'active', 1)",
            vec![
                Value::from(&s),
                Value::from(format!("{edge}{tag}")),
                id(object),
                Value::from(format!("ent{tag}")),
            ],
        ));
    }
    for (edge, memory) in [("shared", a), ("shared", b), ("solo", a)] {
        statements.push(sql(
            "INSERT INTO edge_source (scope_id, edge_id, memory_id) VALUES (?1, ?2, ?3)",
            vec![
                Value::from(&s),
                Value::from(format!("{edge}{tag}")),
                Value::from(memory.to_string()),
            ],
        ));
    }
    node.handle()
        .txn(statements)
        .await
        .expect("the graph rows commit");
}

async fn status_of(node: &common::Node, scope: &Scope, edge: &str) -> Option<String> {
    #[derive(serde::Deserialize)]
    struct Row {
        status: String,
    }
    let rows: Vec<Row> = node
        .handle()
        .query_consistent(
            "SELECT status FROM edge WHERE scope_id = ?1 AND edge_id = ?2".to_owned(),
            vec![Value::from(&ScopeId::of(scope)), Value::from(edge)],
        )
        .await
        .expect("an edge reads");
    rows.into_iter().next().map(|r| r.status)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr006_erasing_a_memory_removes_its_mentions_and_marks_edges_that_lost_every_source() {
    let node = common::node().await;
    let alice = common::scope("alice");
    let first = capture(
        &node,
        &common::memory(&alice, "Dana works at Acme", 1_700_000_000),
    )
    .await;
    let second = capture(
        &node,
        &common::memory(&alice, "Dana works at Acme too", 1_700_000_001),
    )
    .await;
    seed_graph(&node, &alice, first, second, "").await;
    let s = Value::from(&ScopeId::of(&alice));

    // The positive control: the rows are there before the erasure.
    assert_eq!(
        common::count(&node, "SELECT COUNT(*) AS count FROM mention", vec![]).await,
        2
    );
    assert_eq!(
        status_of(&node, &alice, "solo").await.as_deref(),
        Some("active")
    );

    let authority = Authority::of(common::sub("alice")).because("subject_request");
    let erased = Eraser::new()
        .erase(
            &node.handle(),
            &node.ledger,
            &Erasure::new(&alice, first, &authority, UnixSeconds::new(1_700_100_000)),
        )
        .await
        .expect("the erasure runs");
    assert!(erased.removed_derivatives >= 4, "{erased:?}");

    let count = |sql: &'static str, extra: Vec<Value>| {
        let node = &node;
        let mut params = vec![s.clone()];
        params.extend(extra);
        async move { common::count(node, sql, params).await }
    };
    assert_eq!(
        count(
            "SELECT COUNT(*) AS count FROM mention WHERE scope_id = ?1 AND memory_id = ?2",
            vec![Value::from(first.to_string())]
        )
        .await,
        0,
        "its mentions are gone"
    );
    assert_eq!(
        count(
            "SELECT COUNT(*) AS count FROM mention WHERE scope_id = ?1",
            vec![]
        )
        .await,
        1,
        "the other memory's mention stands"
    );
    assert_eq!(
        count(
            "SELECT COUNT(*) AS count FROM extraction WHERE scope_id = ?1 AND memory_id = ?2",
            vec![Value::from(first.to_string())]
        )
        .await,
        0
    );
    assert_eq!(
        count(
            "SELECT COUNT(*) AS count FROM edge_source WHERE scope_id = ?1 AND edge_id = 'shared'",
            vec![]
        )
        .await,
        1,
        "derived_from is reduced by the erased memory"
    );
    assert_eq!(
        status_of(&node, &alice, "shared").await.as_deref(),
        Some("active")
    );
    assert_eq!(
        status_of(&node, &alice, "solo").await.as_deref(),
        Some("origin_erased"),
        "an edge whose last source was erased is marked, and kept"
    );

    // The second memory's erasure takes the last source of `shared` too.
    Eraser::new()
        .erase(
            &node.handle(),
            &node.ledger,
            &Erasure::new(&alice, second, &authority, UnixSeconds::new(1_700_100_001)),
        )
        .await
        .expect("the second erasure runs");
    assert_eq!(
        status_of(&node, &alice, "shared").await.as_deref(),
        Some("origin_erased")
    );
    assert_eq!(
        count(
            "SELECT COUNT(*) AS count FROM edge WHERE scope_id = ?1",
            vec![]
        )
        .await,
        2,
        "no edge row is deleted"
    );
    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr006_scope_erasure_removes_the_scopes_whole_graph_and_no_other() {
    let node = common::node().await;
    let alice = common::scope("alice");
    let bob = common::scope("bob");
    let a1 = capture(
        &node,
        &common::memory(&alice, "Dana works at Acme", 1_700_000_000),
    )
    .await;
    let a2 = capture(
        &node,
        &common::memory(&alice, "Dana works at Acme too", 1_700_000_001),
    )
    .await;
    let b1 = capture(
        &node,
        &common::memory(&bob, "Dana works at Acme", 1_700_000_002),
    )
    .await;
    let b2 = capture(
        &node,
        &common::memory(&bob, "Dana works at Acme too", 1_700_000_003),
    )
    .await;
    seed_graph(&node, &alice, a1, a2, "-a").await;
    seed_graph(&node, &bob, b1, b2, "-b").await;

    Eraser::new()
        .erase_scope(
            &node.handle(),
            &node.ledger,
            &alice,
            &Authority::of(common::sub("alice")).because("subject_request"),
            100,
            UnixSeconds::new(1_700_100_000),
        )
        .await
        .expect("the scope erasure runs");

    let alice_id = Value::from(&ScopeId::of(&alice));
    let bob_id = Value::from(&ScopeId::of(&bob));
    for table in [
        "entity",
        "entity_alias",
        "mention",
        "edge",
        "edge_source",
        "extraction",
    ] {
        let sql: &'static str = match table {
            "entity" => "SELECT COUNT(*) AS count FROM entity WHERE scope_id = ?1",
            "entity_alias" => "SELECT COUNT(*) AS count FROM entity_alias WHERE scope_id = ?1",
            "mention" => "SELECT COUNT(*) AS count FROM mention WHERE scope_id = ?1",
            "edge" => "SELECT COUNT(*) AS count FROM edge WHERE scope_id = ?1",
            "edge_source" => "SELECT COUNT(*) AS count FROM edge_source WHERE scope_id = ?1",
            _ => "SELECT COUNT(*) AS count FROM extraction WHERE scope_id = ?1",
        };
        assert_eq!(
            common::count(&node, sql, vec![alice_id.clone()]).await,
            0,
            "{table}"
        );
        assert!(
            common::count(&node, sql, vec![bob_id.clone()]).await > 0,
            "{table} of another scope is untouched"
        );
    }
    node.shutdown().await;
}
