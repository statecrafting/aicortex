//! The trace, persisted traces, and freshness signals (spec 018 B-5, B-6,
//! B-11; FR-007 to FR-009).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

mod common;

use aicortex_recall::{
    Channel, Delivery, DeliveryConfig, DeliveryRequest, Filter, Query, TraceStore,
};
use common::{Doc, NOW, add, corpus, n_of, numbers, recall, scope, set_status};
use rahi_types::UnixSeconds;

const TOKEN: &str = "zanzibar-quokka-7731";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr007_a_persisted_trace_holds_no_memory_body() {
    let node = common::migrated().await;
    let store = node.handle();
    let (alice, alice_id) = scope("alice");
    corpus(&store, &alice).await;
    let marked = add(
        &store,
        &alice,
        &Doc::new(30, &format!("apple note carrying {TOKEN}"), 0),
    )
    .await;
    common::add_entity(
        &store,
        &alice_id,
        "ent-marked",
        "Zanzibar Quokka",
        &[marked],
    )
    .await;

    let result = recall(&node)
        .recall(&Query::new(alice, format!("apple {TOKEN}"), 5))
        .await
        .unwrap();
    assert!(
        numbers(&result).contains(&30),
        "the marked memory is recalled"
    );
    let persisted = result.trace.to_json().unwrap();
    assert!(!persisted.contains(TOKEN), "{persisted}");
    assert!(
        !persisted.contains("quokka"),
        "the query text is not stored"
    );
    assert!(!persisted.contains("Zanzibar"), "no entity name");
    assert!(persisted.contains(&result.trace.query_hash));
    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_trace_is_always_computed_and_explain_only_trims_it() {
    let node = common::migrated().await;
    let store = node.handle();
    let (alice, _) = scope("alice");
    corpus(&store, &alice).await;
    let full = recall(&node)
        .recall(&Query::new(alice.clone(), "apple", 3))
        .await
        .unwrap();
    let mut brief_query = Query::new(alice, "apple", 3);
    brief_query.explain = false;
    let brief = recall(&node).recall(&brief_query).await.unwrap();

    assert_eq!(numbers(&full), numbers(&brief));
    assert_eq!(full.trace.query_hash, brief.trace.query_hash);
    for (a, b) in full.trace.per_channel.iter().zip(&brief.trace.per_channel) {
        assert_eq!(a.count, b.count, "{:?}", a.channel);
        assert!(!a.candidates.is_empty() || a.count == 0);
        assert!(b.candidates.is_empty());
    }
    assert!(brief.trace.fused.iter().all(|e| e.contributions.is_empty()));
    assert!(brief.trace.fused.iter().all(|e| !e.reason.is_empty()));
    assert!(full.trace.fused.iter().all(|e| !e.contributions.is_empty()));
    // The winning chunk's range travels with the result (B-4).
    let first = &full.memories[0];
    assert!(first.chunk.is_some_and(|range| range.end > range.start));
    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn filters_are_predicates_in_every_channel() {
    let node = common::migrated().await;
    let store = node.handle();
    let (alice, _) = scope("alice");
    corpus(&store, &alice).await;
    let mut query = Query::new(alice, "apple boat", 10);
    query.filter = Filter {
        sources: vec!["mail".to_owned()],
        ..Filter::default()
    };
    let result = recall(&node).recall(&query).await.unwrap();
    // Only memory 5 came from `mail`; every channel agrees.
    for traced in &result.trace.per_channel {
        assert!(
            traced.candidates.iter().all(|c| n_of(c.memory_id) == 5),
            "{:?}",
            traced.channel
        );
    }
    assert_eq!(numbers(&result), vec![5]);

    let mut bad = Query::new(scope("alice").0, "apple", 3);
    bad.filter.trusts = vec!["root".to_owned()];
    assert!(recall(&node).recall(&bad).await.is_err());
    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr008_freshness_sees_past_the_top_k_and_follows_erasure() {
    let node = common::migrated().await;
    let store = node.handle();
    let (alice, alice_id) = scope("alice");
    // Dana is mentioned by three memories: one old and strongly relevant, two
    // newer that the query's own words do not match.
    let relevant = add(&store, &alice, &Doc::new(1, "apple harvest notes", 9)).await;
    let newer = add(&store, &alice, &Doc::new(2, "unrelated filing", 3)).await;
    let newest = add(&store, &alice, &Doc::new(3, "another filing", 1)).await;
    let other = add(&store, &alice, &Doc::new(4, "boat apple orchard", 0)).await;
    common::add_entity(
        &store,
        &alice_id,
        "ent-dana",
        "Dana Whitfield",
        &[relevant, newer, newest],
    )
    .await;
    common::add_entity(&store, &alice_id, "ent-orchard", "orchard", &[other]).await;

    let mut query = Query::new(alice.clone(), "what is new with Dana Whitfield apple", 1);
    query.channels = aicortex_recall::ChannelSet::of([Channel::Text, Channel::Vector]);
    let result = recall(&node).recall(&query).await.unwrap();
    assert_eq!(result.memories.len(), 1);
    let entities = &result.freshness().entities;
    assert_eq!(entities.len(), 1, "{entities:?}");
    assert_eq!(entities[0].entity_id, "ent-dana");
    assert_eq!(entities[0].newest.get(), NOW - common::DAY);
    assert_eq!(entities[0].count, 3);
    assert!(!entities.iter().any(|e| e.entity_id == "ent-orchard"));

    set_status(&store, &alice_id, newest, "erased").await;
    let after = recall(&node).recall(&query).await.unwrap();
    let entities = &after.freshness().entities;
    assert_eq!(entities[0].newest.get(), NOW - 3 * common::DAY);
    assert_eq!(entities[0].count, 2);

    let sources = &after.freshness().sources;
    assert!(sources.iter().any(|s| s.system == "notes" && s.count == 3));
    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr009_freshness_holds_no_body_name_or_alias_and_is_repeatable() {
    let node = common::migrated().await;
    let store = node.handle();
    let (alice, alice_id) = scope("alice");
    let marked = add(
        &store,
        &alice,
        &Doc::new(1, &format!("apple note carrying {TOKEN}"), 2),
    )
    .await;
    common::add_entity(&store, &alice_id, "ent-q", "Quokka Marshall", &[marked]).await;
    let mut query = Query::new(alice, format!("apple Quokka Marshall {TOKEN}"), 5);
    query.filter.sources = vec!["notes".to_owned(), "mail".to_owned()];
    let first = recall(&node).recall(&query).await.unwrap();
    let second = recall(&node).recall(&query).await.unwrap();

    let block = serde_json::to_string(first.freshness()).unwrap();
    assert!(!first.freshness().entities.is_empty());
    assert!(!first.freshness().sources.is_empty());
    for needle in [TOKEN, "Quokka", "quokka", "Marshall", "apple note"] {
        assert!(!block.contains(needle), "{needle} in {block}");
    }
    assert_eq!(block, serde_json::to_string(second.freshness()).unwrap());
    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_filter_field_narrows_every_channel() {
    let node = common::migrated().await;
    let store = node.handle();
    let (alice, alice_id) = scope("alice");
    corpus(&store, &alice).await;
    common::add_entity(
        &store,
        &alice_id,
        "ent-orchard",
        "orchard",
        &[common::memory_id(2)],
    )
    .await;
    let run = |filter: Filter| {
        let alice = alice.clone();
        let node = &node;
        async move {
            let mut query = Query::new(alice, "apple orchard boat", 10);
            query.filter = filter;
            let result = recall(node).recall(&query).await.unwrap();
            numbers(&result)
        }
    };
    let day = |days: u64| Some(rahi_types::UnixSeconds::new(NOW - days * common::DAY));
    let from = run(Filter {
        created_from: day(5),
        ..Filter::default()
    })
    .await;
    assert!(from.iter().all(|n| [2, 3, 4].contains(n)), "{from:?}");
    let to = run(Filter {
        created_to: day(10),
        ..Filter::default()
    })
    .await;
    assert!(to.iter().all(|n| [1, 5].contains(n)), "{to:?}");
    let entity = run(Filter {
        entities: vec!["ent-orchard".to_owned()],
        ..Filter::default()
    })
    .await;
    assert_eq!(entity, vec![2]);
    let human = run(Filter {
        actor_kinds: vec!["human".to_owned()],
        trusts: vec!["assertion".to_owned()],
        kinds: vec!["observation".to_owned()],
        ..Filter::default()
    })
    .await;
    assert_eq!(human.len(), 5);
    let agent = run(Filter {
        actor_kinds: vec!["agent".to_owned()],
        ..Filter::default()
    })
    .await;
    assert!(agent.is_empty(), "{agent:?}");
    node.shutdown().await;
}

/// 018 B-6: a trace is stored against its scope, keyed by its query hash, read
/// back inside its retention window and not after it, and never holds a body.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b6_a_stored_trace_reads_back_inside_its_window_and_scope_only() {
    let node = common::migrated().await;
    let store = node.handle();
    let (alice, alice_id) = scope("alice");
    let (_, bob_id) = scope("bob");
    corpus(&store, &alice).await;
    let marked = add(
        &store,
        &alice,
        &Doc::new(30, &format!("apple note carrying {TOKEN}"), 0),
    )
    .await;
    let result = recall(&node)
        .recall(&Query::new(alice.clone(), format!("apple {TOKEN}"), 5))
        .await
        .unwrap();
    assert!(numbers(&result).contains(&n_of(marked)));
    let delivery = Delivery::new(store.clone(), DeliveryConfig::default())
        .deliver(
            &alice,
            &result,
            &DeliveryRequest::parse(Some("chunk"), None, None).unwrap(),
        )
        .await
        .unwrap()
        .expect("the request asked for delivery");

    let now = UnixSeconds::new(NOW);
    let id = TraceStore::record(
        &store,
        &alice_id,
        &result.trace,
        Some(&delivery.report),
        now,
        100,
    )
    .await
    .unwrap();

    let hash = &result.trace.query_hash;
    let read = TraceStore::by_hash(&store, &alice_id, hash, now, 10)
        .await
        .unwrap();
    assert_eq!(read.len(), 1);
    assert_eq!(read[0].trace_id, id);
    assert_eq!(read[0].expires_at, NOW + 100);
    assert_eq!(
        read[0].trace,
        serde_json::from_str::<serde_json::Value>(&result.trace.to_json().unwrap()).unwrap()
    );
    let stored = serde_json::to_string(&read[0]).unwrap_or_default();
    assert!(!stored.contains(TOKEN), "no body in the stored trace");
    // 025 B-5: the stored delivery report names the counter it enforced.
    let counter = &read[0].delivery.as_ref().unwrap()["counter"];
    assert_eq!(counter["id"], "utf8-bytes-div3");
    assert_eq!(counter["version"], 1);

    let other_scope = TraceStore::by_hash(&store, &bob_id, hash, now, 10)
        .await
        .unwrap();
    assert!(other_scope.is_empty(), "another scope reads none");
    let expired = TraceStore::by_hash(&store, &alice_id, hash, UnixSeconds::new(NOW + 100), 10)
        .await
        .unwrap();
    assert!(expired.is_empty(), "the window is exclusive at its end");
    node.shutdown().await;
}

/// 018 B-6: recording a trace removes the scope's expired ones, and a window
/// of zero seconds is refused because nobody could read it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b6_recording_prunes_expired_traces_and_refuses_an_empty_window() {
    let node = common::migrated().await;
    let store = node.handle();
    let (alice, alice_id) = scope("alice");
    corpus(&store, &alice).await;
    let result = recall(&node)
        .recall(&Query::new(alice, "apple", 3))
        .await
        .unwrap();
    #[derive(serde::Deserialize)]
    struct Count {
        count: i64,
    }
    let count = || async {
        let rows: Vec<Count> = store
            .query(
                "SELECT COUNT(*) AS count FROM recall_trace WHERE scope_id = ?1".to_owned(),
                vec![rahi_store::Value::from(alice_id.as_str())],
            )
            .await
            .unwrap();
        rows[0].count
    };

    TraceStore::record(
        &store,
        &alice_id,
        &result.trace,
        None,
        UnixSeconds::new(NOW),
        10,
    )
    .await
    .unwrap();
    assert_eq!(count().await, 1);
    TraceStore::record(
        &store,
        &alice_id,
        &result.trace,
        None,
        UnixSeconds::new(NOW + 1_000),
        10,
    )
    .await
    .unwrap();
    assert_eq!(count().await, 1, "the expired trace was removed");

    let refused = TraceStore::record(
        &store,
        &alice_id,
        &result.trace,
        None,
        UnixSeconds::new(NOW),
        0,
    )
    .await
    .unwrap_err();
    assert!(
        refused.message().contains("at least one second"),
        "{refused}"
    );
    node.shutdown().await;
}
