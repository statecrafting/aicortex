//! The extraction job: retry, dead letter, scheduling, and what preflight
//! reports (spec 017 B-7, AC-2).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

mod common;

use std::time::Duration;

use aicortex_graph::{ExtractionConfig, PatternExtractor, read_scope, schedule};
use rahi_store::RetryPolicy;
use rahi_types::UnixSeconds;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b7_a_failing_job_is_retried_then_dead_lettered_and_preflight_counts_it() {
    let node = common::migrated().await;
    let store = node.handle();
    let (scope, scope_id) = common::scope("alice");
    let bad = common::add_corrupt_memory(&store, &scope).await;
    let config = ExtractionConfig {
        retry: RetryPolicy {
            max_attempts: 1,
            base: Duration::from_secs(2),
            cap: Duration::from_secs(2),
        },
        ..ExtractionConfig::standard()
    };
    let report = common::extract_with(&store, &scope_id, bad, PatternExtractor, config).await;
    assert_eq!((report.claimed, report.completed, report.dead), (1, 0, 1));

    let graph = read_scope(&store, &scope_id).await.unwrap();
    assert_eq!(
        graph.dead, 1,
        "the dead letter is visible to preflight (AC-2)"
    );
    assert_eq!(graph.pending, 0);
    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ac2_preflight_counts_entities_and_edges_per_scope_and_rejected_edges() {
    let node = common::migrated().await;
    let store = node.handle();
    let (alice, alice_id) = common::scope("alice");
    let (bob, bob_id) = common::scope("bob");
    let a = common::add_memory(
        &store,
        &alice,
        "Dana Whitfield --mentors--> Priya Rao. Dana Whitfield works at Acme Corp.",
        1_700_000_000,
    )
    .await;
    common::extract(&store, &alice_id, a).await;
    let empty = read_scope(&store, &bob_id).await.unwrap();
    assert_eq!((empty.entities, empty.edges), (0, 0));
    let _ = bob;

    let graph = read_scope(&store, &alice_id).await.unwrap();
    // The explicit `--predicate-->` form types both ends `concept` unless the
    // scope already knows them, so Dana Whitfield is a concept and a person.
    assert_eq!(
        graph.entities, 4,
        "Dana (concept), Dana (person), Priya Rao, Acme Corp"
    );
    assert_eq!(graph.edges, 1, "only works_at is in the vocabulary");
    assert_eq!(graph.rejected_edges, 1, "mentors was refused and counted");
    assert_eq!(graph.dead, 0);
    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b7_scheduling_stages_each_missing_extraction_once() {
    let node = common::migrated().await;
    let store = node.handle();
    let (scope, scope_id) = common::scope("alice");
    let m = common::add_memory(
        &store,
        &scope,
        "Dana Whitfield works on Apollo.",
        1_700_000_000,
    )
    .await;
    let now = UnixSeconds::new(1_700_000_050);
    let first = schedule(&store, &scope_id, &PatternExtractor, None, 10, now)
        .await
        .unwrap();
    assert_eq!(first.staged, 1);
    let second = schedule(&store, &scope_id, &PatternExtractor, None, 10, now)
        .await
        .unwrap();
    assert_eq!(
        second.staged, 0,
        "staging is idempotent under the processing key"
    );
    assert_eq!(common::extract(&store, &scope_id, m).await.completed, 1);
    let third = schedule(&store, &scope_id, &PatternExtractor, None, 10, now)
        .await
        .unwrap();
    assert_eq!(
        third.staged, 0,
        "an extracted memory is not scheduled again"
    );
    node.shutdown().await;
}
