//! A scope erasure removes the scope's persisted recall traces and no other
//! scope's (spec 018 B-6, 014 B-8).
//!
//! The rows are written directly, as `aicortex-recall`'s trace store would, so
//! this crate proves its own half of the obligation: the table exists at the
//! expected schema version and the erasure reaches it.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

mod common;

use aicortex_store::{
    Authority, EXPECTED_SCHEMA_VERSION, Eraser, Lifecycle, RECALL_TRACE_VERSION, ScopeId,
};
use aicortex_types::{Memory, Scope};
use rahi_store::{Statement, TxnBuilder, Value};
use rahi_types::UnixSeconds;

const COUNT: &str = "SELECT COUNT(*) AS count FROM recall_trace WHERE scope_id = ?1";

async fn capture(node: &common::Node, memory: &Memory) {
    let admitted = common::admit(memory);
    let work = common::work(memory);
    let mut txn = TxnBuilder::new();
    Lifecycle::new()
        .capture(&node.handle(), &mut txn, &admitted, &work)
        .await
        .expect("the capture stages");
    node.handle()
        .txn(txn.into_statements())
        .await
        .expect("the capture commits");
}

async fn trace(node: &common::Node, scope: &Scope, id: &str) {
    node.handle()
        .txn(vec![Statement::with_params(
            "INSERT INTO recall_trace
             (trace_id, scope_id, query_hash, recorded_at, expires_at, trace)
             VALUES (?1, ?2, 'hash', 1700000000, 1800000000, '{}')"
                .to_owned(),
            vec![Value::from(id), Value::from(&ScopeId::of(scope))],
        )])
        .await
        .expect("the trace row commits");
}

#[test]
fn the_trace_table_is_no_newer_than_the_expected_schema_version() {
    // Later specs append after it (023's review tables), so the expected
    // version is the newest and this one only has to be within the list.
    const { assert!(RECALL_TRACE_VERSION <= EXPECTED_SCHEMA_VERSION) };
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scope_erasure_removes_the_scopes_traces_and_no_other() {
    let node = common::node().await;
    let alice = common::scope("alice");
    let bob = common::scope("bob");
    capture(
        &node,
        &common::memory(&alice, "Dana works at Acme", 1_700_000_000),
    )
    .await;
    capture(
        &node,
        &common::memory(&bob, "Dana works at Acme", 1_700_000_001),
    )
    .await;
    trace(&node, &alice, "trace-a").await;
    trace(&node, &bob, "trace-b").await;

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
    assert_eq!(common::count(&node, COUNT, vec![alice_id]).await, 0);
    assert_eq!(common::count(&node, COUNT, vec![bob_id]).await, 1);
    node.shutdown().await;
}
