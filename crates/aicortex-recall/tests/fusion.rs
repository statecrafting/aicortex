//! Fusion, channels, scope, status, and budget (spec 018 B-2, B-3, B-7 to B-9;
//! FR-001 to FR-006).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

mod common;

use aicortex_recall::{Channel, ChannelSet, Extra, Query, recompute_order};
use aicortex_store::ScopeId;
use common::{Doc, add, corpus, n_of, numbers, recall, scope, set_status};
use rahi_store::{Statement, Value};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Case {
    name: String,
    text: String,
    channels: Vec<String>,
    k: usize,
    expected: Vec<u64>,
}

fn channels_of(names: &[String]) -> ChannelSet {
    ChannelSet::of(names.iter().map(|name| match name.as_str() {
        "vector" => Channel::Vector,
        "text" => Channel::Text,
        "recency" => Channel::Recency,
        "graph" => Channel::Graph,
        other => panic!("unknown channel {other}"),
    }))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr001_the_returned_order_matches_the_committed_fixtures() {
    let node = common::migrated().await;
    let store = node.handle();
    let (alice, _) = scope("alice");
    corpus(&store, &alice).await;
    let cases: Vec<Case> =
        serde_json::from_str(include_str!("../testdata/queries/core.json")).unwrap();
    assert!(!cases.is_empty());
    for case in cases {
        let mut query = Query::new(alice.clone(), case.text.as_str(), case.k);
        query.channels = channels_of(&case.channels);
        let result = recall(&node).recall(&query).await.unwrap();
        assert_eq!(numbers(&result), case.expected, "{}", case.name);
        // B-8: the same query again, the same order and the same trace.
        let again = recall(&node).recall(&query).await.unwrap();
        assert_eq!(numbers(&again), case.expected, "{} (second run)", case.name);
        assert_eq!(
            result.trace.to_json().unwrap().replace(
                &format!("\"budget_used_ms\":{}", result.trace.budget_used_ms),
                ""
            ),
            again.trace.to_json().unwrap().replace(
                &format!("\"budget_used_ms\":{}", again.trace.budget_used_ms),
                ""
            ),
            "{}",
            case.name
        );
    }
    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr002_disabling_a_channel_moves_the_order_and_one_channel_returns_its_own() {
    let node = common::migrated().await;
    let store = node.handle();
    let (alice, _) = scope("alice");
    corpus(&store, &alice).await;

    let all = recall(&node)
        .recall(&Query::new(alice.clone(), "apple", 5))
        .await
        .unwrap();
    let mut without_recency = Query::new(alice.clone(), "apple", 5);
    without_recency.channels = ChannelSet::of([Channel::Vector, Channel::Text, Channel::Graph]);
    let without = recall(&node).recall(&without_recency).await.unwrap();
    // Memory 3 (about a boat, not an apple) is only helped by recency:
    // disabling the channel lowers its fused score.
    let score_of = |result: &aicortex_recall::RecallResult, n: u64| {
        result
            .trace
            .fused
            .iter()
            .find(|entry| n_of(entry.memory_id) == n)
            .unwrap()
            .score
    };
    assert!(score_of(&all, 3) > score_of(&without, 3));
    assert_eq!(numbers(&all), vec![2, 4, 1, 3, 5]);
    assert_eq!(numbers(&without), vec![2, 1, 4, 5, 3]);

    for (channel, enabled) in [
        (Channel::Vector, ChannelSet::of([Channel::Vector])),
        (Channel::Text, ChannelSet::of([Channel::Text])),
        (Channel::Recency, ChannelSet::of([Channel::Recency])),
    ] {
        let mut query = Query::new(alice.clone(), "apple", 5);
        query.channels = enabled;
        let result = recall(&node).recall(&query).await.unwrap();
        let own: Vec<u64> = result
            .trace
            .per_channel
            .iter()
            .find(|traced| traced.channel == channel)
            .unwrap()
            .candidates
            .iter()
            .map(|c| n_of(c.memory_id))
            .collect();
        assert_eq!(numbers(&result), own, "{channel:?} alone");
    }

    let mut none = Query::new(alice.clone(), "apple", 5);
    none.channels = ChannelSet::of([]);
    assert!(
        recall(&node)
            .recall(&none)
            .await
            .unwrap()
            .memories
            .is_empty()
    );
    let mut zero = Query::new(alice, "apple", 0);
    zero.channels = ChannelSet::all();
    assert!(
        recall(&node)
            .recall(&zero)
            .await
            .unwrap()
            .memories
            .is_empty()
    );
    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr003_quarantined_and_erased_are_absent_from_every_channel() {
    let node = common::migrated().await;
    let store = node.handle();
    let (alice, alice_id) = scope("alice");
    corpus(&store, &alice).await;
    // Twins of memory 2 in every channel: apple text and vector, recent, and
    // mentioning the entity the query names.
    let live = add(&store, &alice, &Doc::new(10, "apple twin of orchard", 0)).await;
    let quarantined = add(&store, &alice, &Doc::new(11, "apple twin of orchard", 0)).await;
    let erased = add(&store, &alice, &Doc::new(12, "apple twin of orchard", 0)).await;
    let superseded = add(&store, &alice, &Doc::new(13, "apple twin of orchard", 0)).await;
    common::add_entity(
        &store,
        &alice_id,
        "ent-apple",
        "apple",
        &[live, quarantined, erased, superseded],
    )
    .await;
    set_status(&store, &alice_id, quarantined, "quarantined").await;
    set_status(&store, &alice_id, erased, "erased").await;
    set_status(&store, &alice_id, superseded, "superseded").await;

    let result = recall(&node)
        .recall(&Query::new(alice.clone(), "apple", 20))
        .await
        .unwrap();
    for traced in &result.trace.per_channel {
        let ids: Vec<u64> = traced
            .candidates
            .iter()
            .map(|c| n_of(c.memory_id))
            .collect();
        assert!(!ids.is_empty(), "{:?} proposes something", traced.channel);
        assert!(ids.contains(&10), "{:?} {ids:?}", traced.channel);
        for hidden in [11, 12, 13] {
            assert!(!ids.contains(&hidden), "{:?} {ids:?}", traced.channel);
        }
    }

    // B-7: superseded may be asked for, and the trace records it; there is no
    // way to ask for the other two.
    let mut widened = Query::new(alice, "apple", 20);
    widened.filter.extra_statuses = vec![Extra::Superseded];
    let result = recall(&node).recall(&widened).await.unwrap();
    for traced in &result.trace.per_channel {
        let ids: Vec<u64> = traced
            .candidates
            .iter()
            .map(|c| n_of(c.memory_id))
            .collect();
        assert!(ids.contains(&13), "{:?} {ids:?}", traced.channel);
        assert!(!ids.contains(&11) && !ids.contains(&12), "{ids:?}");
    }
    assert_eq!(result.trace.filters.extra_statuses, vec![Extra::Superseded]);
    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr004_another_scopes_identical_content_is_never_returned() {
    let node = common::migrated().await;
    let store = node.handle();
    let (alice, _) = scope("alice");
    let (bob, bob_id) = scope("bob");
    corpus(&store, &alice).await;
    // Bob holds the same text at the same ages, under other ids, and an
    // entity of the same name.
    let mut bobs = Vec::new();
    for doc in [
        Doc::new(21, "apple pie recipe with cinnamon", 1),
        Doc::new(22, "visit to the apple orchard", 0),
    ] {
        bobs.push(add(&store, &bob, &doc).await);
    }
    common::add_entity(&store, &bob_id, "ent-bob-apple", "apple", &bobs).await;

    let result = recall(&node)
        .recall(&Query::new(alice, "apple", 20))
        .await
        .unwrap();
    for traced in &result.trace.per_channel {
        for candidate in &traced.candidates {
            assert!(
                !bobs.contains(&candidate.memory_id),
                "{:?} leaked a memory of another scope",
                traced.channel
            );
        }
    }
    assert!(!numbers(&result).iter().any(|n| *n >= 20));
    assert!(result.freshness().entities.is_empty());
    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr005_the_fused_arithmetic_reproduces_the_returned_order() {
    let node = common::migrated().await;
    let store = node.handle();
    let (alice, alice_id) = scope("alice");
    corpus(&store, &alice).await;
    let linked = [common::memory_id(2), common::memory_id(4)];
    common::add_entity(&store, &alice_id, "ent-apple", "apple", &linked).await;
    let result = recall(&node)
        .recall(&Query::new(alice, "apple boat", 5))
        .await
        .unwrap();
    assert!(result.memories.len() >= 3);
    let recomputed: Vec<u64> = recompute_order(&result.trace)
        .into_iter()
        .map(n_of)
        .collect();
    assert_eq!(recomputed, numbers(&result));
    for entry in &result.trace.fused {
        let sum: f64 = entry.contributions.iter().map(|c| c.term).sum();
        assert_eq!(entry.score, sum * entry.prior);
        for c in &entry.contributions {
            assert_eq!(
                c.term,
                c.weight / (f64::from(result.trace.rank_constant) + f64::from(c.rank))
            );
        }
    }
    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr006_a_one_millisecond_budget_returns_a_truncated_result_not_an_error() {
    let node = common::migrated().await;
    let store = node.handle();
    let (alice, alice_id) = scope("alice");
    corpus(&store, &alice).await;
    // More vector rows than one page of the scan reads.
    let mut batch = Vec::new();
    let vector: Vec<u8> = [1.0_f32, 0.0]
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect();
    for n in 1000..3200_u64 {
        let id = common::memory_id(n).to_string();
        batch.push(Statement::with_params(
            "INSERT INTO memory
             (id, scope_id, status, kind, trust, fingerprint, schema_version, body_bytes,
              created, updated, record)
             VALUES (?1, ?2, 'active', 'observation', 'assertion', ?1, 1, 1, 1700000000,
                     1700000000, '{}')",
            vec![Value::from(id.as_str()), Value::from(alice_id.as_str())],
        ));
        batch.push(Statement::with_params(
            "INSERT INTO chunk
             (chunk_id, scope_id, memory_id, model_revision, ordinal, byte_start, byte_end)
             VALUES (?1, ?2, ?3, 1, 0, 0, 1)",
            vec![
                Value::from(format!("bulk-{n}")),
                Value::from(alice_id.as_str()),
                Value::from(id.as_str()),
            ],
        ));
        batch.push(Statement::with_params(
            "INSERT INTO embedding
             (scope_id, memory_id, model_id, model_revision, chunk_ordinal, dims,
              normalized, vector, updated)
             VALUES (?1, ?2, ?3, 1, 0, 2, 1, ?4, 1700000000)",
            vec![
                Value::from(alice_id.as_str()),
                Value::from(id.as_str()),
                Value::from(common::MODEL),
                Value::Blob(vector.clone()),
            ],
        ));
        if batch.len() >= 600 {
            store.txn(std::mem::take(&mut batch)).await.unwrap();
        }
    }
    if !batch.is_empty() {
        store.txn(batch).await.unwrap();
    }

    let mut query = Query::new(alice, "apple", 5);
    query.budget = std::time::Duration::from_millis(1);
    let result = recall(&node).recall(&query).await.unwrap();
    assert!(!result.memories.is_empty());
    assert!(result.truncated());
    let vector = result
        .trace
        .per_channel
        .iter()
        .find(|traced| traced.channel == Channel::Vector)
        .unwrap();
    assert!(vector.truncated);
    let _ = ScopeId::of;
    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_importance_prior_scales_the_fused_score_and_never_zeroes_it() {
    let node = common::migrated().await;
    let store = node.handle();
    let (alice, _) = scope("alice");
    // Identical text and age, so identical ranks except the id tie-break:
    // memory 1 would win, and a higher importance on memory 2 overturns it.
    add(
        &store,
        &alice,
        &Doc::new(1, "apple twin", 1).importance(0.1),
    )
    .await;
    add(
        &store,
        &alice,
        &Doc::new(2, "apple twin", 1).importance(0.9),
    )
    .await;
    let mut query = Query::new(alice, "apple", 2);
    query.channels = ChannelSet::of([Channel::Text]);
    let result = recall(&node).recall(&query).await.unwrap();
    assert_eq!(numbers(&result), vec![2, 1]);
    let low = result.trace.fused[1].prior;
    let high = result.trace.fused[0].prior;
    assert!(0.0 < low && low < high && high < 1.5, "{low} {high}");
    node.shutdown().await;
}
