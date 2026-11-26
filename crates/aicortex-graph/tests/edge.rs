//! Typed edges, versioned re-extraction, the bounded walk, and contradiction
//! (spec 017 B-4, B-6, B-7, B-9, B-10; FR-003, FR-004, FR-005).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

mod common;

use aicortex_graph::{
    EdgeStatus, ExtractionConfig, HopEnd, Predicate, WalkOptions, contradictions, edges_of,
    neighbors,
};
use common::Versioned;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr003_re_extraction_supersedes_the_prior_version_and_keeps_the_count() {
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

    common::extract_with(
        &store,
        &scope_id,
        m,
        Versioned(1),
        ExtractionConfig::standard(),
    )
    .await;
    let active = |status: &'static str| {
        let store = store.clone();
        let scope_id = scope_id.clone();
        async move {
            common::count(
                &store,
                match status {
                    "active" => "SELECT COUNT(*) AS count FROM edge WHERE scope_id = ?1 AND status = 'active'",
                    _ => "SELECT COUNT(*) AS count FROM edge WHERE scope_id = ?1 AND status = 'superseded'",
                },
                vec![scope_id.as_str().into()],
            )
            .await
        }
    };
    assert_eq!(active("active").await, 1);
    assert_eq!(active("superseded").await, 0);

    let report = common::extract_with(
        &store,
        &scope_id,
        m,
        Versioned(2),
        ExtractionConfig::standard(),
    )
    .await;
    assert_eq!(report.completed, 1);
    assert_eq!(
        active("active").await,
        1,
        "the count of active edges is unchanged"
    );
    assert_eq!(
        active("superseded").await,
        1,
        "the version 1 edge was superseded, not duplicated"
    );

    let dana = common::entity_named(&store, &scope_id, "Dana Whitfield").await;
    let edges = edges_of(&store, &scope_id, &dana, Some(EdgeStatus::Active), 10)
        .await
        .unwrap();
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0].extractor_version, 2);
    assert_eq!(edges[0].extractor, "pattern");
    assert_eq!(edges[0].derived_from, vec![m]);
    assert_eq!(edges[0].predicate, Predicate::WorksOn);
    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b7_extraction_is_idempotent_per_memory_and_version() {
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
    assert_eq!(common::extract(&store, &scope_id, m).await.completed, 1);
    let again = common::extract(&store, &scope_id, m).await;
    assert_eq!(again.claimed, 0, "the finished job is not claimed again");
    assert_eq!(
        common::count(
            &store,
            "SELECT COUNT(*) AS count FROM rahi_processing WHERE processor = 'extract.r1'",
            vec![],
        )
        .await,
        1
    );
    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr004_a_bounded_walk_returns_memories_with_paths_and_ends_on_a_cycle() {
    let node = common::migrated().await;
    let store = node.handle();
    let (scope, scope_id) = common::scope("alice");
    let texts = [
        "Dana Whitfield works on Apollo.",
        "Apollo depends on Hermes.",
        "Hermes is part of Olympus.",
        "Hermes depends on Apollo.",
    ];
    let mut ids = Vec::new();
    for (n, text) in texts.iter().enumerate() {
        let m = common::add_memory(&store, &scope, text, 1_700_000_000 + n as u64).await;
        assert_eq!(common::extract(&store, &scope_id, m).await.completed, 1);
        ids.push(m);
    }
    let dana = common::entity_named(&store, &scope_id, "Dana Whitfield").await;
    let apollo = common::entity_named(&store, &scope_id, "Apollo").await;

    // Depth 2 from Dana: her own edge, then Apollo's two edges. The Olympus
    // edge is a third hop away.
    let hits = neighbors(&store, &scope_id, &dana, &WalkOptions::default())
        .await
        .unwrap();
    let reached: Vec<_> = hits.iter().map(|h| h.memory).collect();
    assert!(reached.contains(&ids[0]) && reached.contains(&ids[1]) && reached.contains(&ids[3]));
    assert!(
        !reached.contains(&ids[2]),
        "depth 2 stops before the third hop"
    );
    let first = hits.iter().find(|h| h.memory == ids[0]).unwrap();
    assert_eq!(first.path.0.len(), 1);
    assert_eq!(first.path.0[0].predicate, Predicate::WorksOn);
    assert_eq!(first.path.0[0].from, dana);
    assert_eq!(first.path.0[0].to, HopEnd::Entity(apollo.clone()));
    let second = hits.iter().find(|h| h.memory == ids[1]).unwrap();
    assert_eq!(
        second.path.0.len(),
        2,
        "the path is the route, not just the last edge"
    );

    // Deeper, on a graph with a cycle between Apollo and Hermes: it ends.
    let deep = neighbors(
        &store,
        &scope_id,
        &dana,
        &WalkOptions {
            depth: 4,
            ..WalkOptions::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(deep.len(), 4, "every memory once, despite the cycle");

    // A fan-out of one follows one edge per node per hop.
    let narrow = neighbors(
        &store,
        &scope_id,
        &apollo,
        &WalkOptions {
            depth: 1,
            fanout: 1,
            ..WalkOptions::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(narrow.len(), 1);
    let wide = neighbors(
        &store,
        &scope_id,
        &apollo,
        &WalkOptions {
            depth: 1,
            ..WalkOptions::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(wide.len(), 3);

    // A walk from another scope's id reaches nothing.
    let (_, other) = common::scope("bob");
    assert!(
        neighbors(&store, &other, &dana, &WalkOptions::default())
            .await
            .unwrap()
            .is_empty()
    );
    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr005_two_contradicting_edges_make_one_record_and_neither_is_deleted() {
    let node = common::migrated().await;
    let store = node.handle();
    let (scope, scope_id) = common::scope("alice");
    let a = common::add_memory(
        &store,
        &scope,
        "Dana Whitfield works at Acme Corp.",
        1_700_000_000,
    )
    .await;
    let b = common::add_memory(
        &store,
        &scope,
        "Dana Whitfield works at Globex.",
        1_700_000_010,
    )
    .await;
    common::extract(&store, &scope_id, a).await;
    assert!(
        contradictions(&store, &scope_id, 10)
            .await
            .unwrap()
            .is_empty()
    );
    common::extract(&store, &scope_id, b).await;

    let found = contradictions(&store, &scope_id, 10).await.unwrap();
    assert_eq!(found.len(), 1, "exactly one contradiction record");
    assert_eq!(found[0].predicate, Predicate::WorksAt);
    assert_eq!(found[0].status, "open");
    assert_eq!(
        common::count(
            &store,
            "SELECT COUNT(*) AS count FROM edge WHERE scope_id = ?1 AND status = 'active'",
            vec![scope_id.as_str().into()],
        )
        .await,
        2,
        "neither edge was deleted or demoted"
    );

    // The same fact stated twice is one edge with two sources, not a clash.
    let c = common::add_memory(
        &store,
        &scope,
        "Dana Whitfield works at Acme Corp.",
        1_700_000_020,
    )
    .await;
    common::extract(&store, &scope_id, c).await;
    assert_eq!(
        contradictions(&store, &scope_id, 10).await.unwrap().len(),
        1
    );
    let dana = common::entity_named(&store, &scope_id, "Dana Whitfield").await;
    let edges = edges_of(&store, &scope_id, &dana, Some(EdgeStatus::Active), 10)
        .await
        .unwrap();
    assert_eq!(edges.len(), 2);
    assert!(edges.iter().any(|e| e.derived_from.len() == 2));
    node.shutdown().await;
}
