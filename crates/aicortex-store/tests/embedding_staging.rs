//! Every memory write stages its embedding work (spec 015 B-1).
//!
//! `MemoryRepo::insert` is the write path spec 053 documents for host
//! applications, and `Lifecycle::capture` is the standalone one. Both take
//! the caller's observation of the active embedding model, and the staged
//! guard makes that observation a commit-time condition:
//!
//! - with a model active, the write stages one job in that revision's
//!   partition, on insert and on merge alike;
//! - with no model active, the write stages no job and the re-embedding pass
//!   that follows activation owns coverage;
//! - a stale observation aborts the whole write, so a memory never commits
//!   next to an active model without its job.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

mod common;

use aicortex_store::{Captured, EmbeddingTarget, Lifecycle, MemoryRepo};
use rahi_store::{StoreHandle, TxnBuilder, Work};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Count {
    count: i64,
}

/// Pending jobs in one model revision's embedding partition.
async fn pending(store: &StoreHandle, processor: &str) -> u64 {
    Work::counts(store)
        .await
        .unwrap()
        .into_iter()
        .find(|queue| queue.processor == processor)
        .map_or(0, |queue| queue.pending)
}

/// Memory rows over every scope: a test-only read proving absence.
async fn memories(store: &StoreHandle) -> i64 {
    let counted: Vec<Count> = store
        .query_consistent("SELECT count(*) AS count FROM memory", vec![])
        .await
        .unwrap();
    counted.first().map(|row| row.count).unwrap_or_default()
}

/// Stage one host-mode insert with `embedding` and submit it.
async fn insert(
    store: &StoreHandle,
    text: &str,
    embedding: &EmbeddingTarget,
) -> Result<(), rahi_types::Error> {
    let memory = common::memory(&common::scope("host"), text, 1_700_000_000);
    let mut txn = TxnBuilder::new();
    MemoryRepo::new()
        .insert(
            &mut txn,
            &common::admit(&memory),
            &memory.provenance,
            embedding,
            &common::work(&memory),
        )
        .expect("the insert stages");
    store.txn(txn.into_statements()).await.map(|_| ())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b1_a_host_insert_stages_embedding_work_for_the_active_model() {
    let fixture = common::Fixture::migrated().await;
    let store = fixture.handle();
    let embedding = EmbeddingTarget::observe(&store).await.unwrap();
    assert_eq!(embedding.model().map(|model| model.revision), Some(1));

    insert(&store, "a host-mode observation", &embedding)
        .await
        .expect("the host insert commits");

    assert_eq!(memories(&store).await, 1);
    assert_eq!(pending(&store, "embed.r1").await, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b1_a_standalone_capture_stages_embedding_work_on_insert_and_merge() {
    let fixture = common::Fixture::migrated().await;
    let store = fixture.handle();
    let memory = common::memory(&common::scope("standalone"), "said twice", 1_700_000_000);
    let mut repeat = common::memory(&common::scope("standalone"), "said twice", 1_700_000_100);
    repeat.provenance = memory.provenance.clone();

    for (capture, expected) in [(&memory, "inserted"), (&repeat, "merged")] {
        let mut txn = TxnBuilder::new();
        let captured = Lifecycle::new()
            .capture(
                &store,
                &mut txn,
                &common::admit(capture),
                &common::work(capture),
            )
            .await
            .expect("the capture stages");
        store
            .txn(txn.into_statements())
            .await
            .expect("the capture commits");
        match (expected, captured) {
            ("inserted", Captured::Inserted(id)) | ("merged", Captured::Merged(id)) => {
                assert_eq!(id, memory.id);
            }
            (expected, captured) => panic!("expected {expected}, got {captured:?}"),
        }
    }

    // One memory, one job: the merge re-stages the same processing identity.
    assert_eq!(memories(&store).await, 1);
    assert_eq!(pending(&store, "embed.r1").await, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b1_without_an_active_model_an_insert_commits_and_stages_no_work() {
    let fixture = common::Fixture::migrated_without_model().await;
    let store = fixture.handle();
    let embedding = EmbeddingTarget::observe(&store).await.unwrap();
    assert_eq!(embedding, EmbeddingTarget::none());

    insert(&store, "captured before first activation", &embedding)
        .await
        .expect("capture stays available before activation");

    assert_eq!(memories(&store).await, 1);
    assert!(
        Work::counts(&store).await.unwrap().is_empty(),
        "no embedding work exists without a model"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b1_a_stale_model_observation_aborts_the_whole_insert() {
    // No model observed, one active at commit.
    let fixture = common::Fixture::migrated_without_model().await;
    let store = fixture.handle();
    let before_activation = EmbeddingTarget::observe(&store).await.unwrap();
    common::activate_model(&store, "test-local", 1).await;
    assert!(
        insert(&store, "observed no model", &before_activation)
            .await
            .is_err(),
        "a write that observed no model must not commit next to an active one"
    );

    // Revision 1 observed, revision 2 active at commit.
    let before_switch = EmbeddingTarget::observe(&store).await.unwrap();
    common::activate_model(&store, "test-local", 2).await;
    assert!(
        insert(&store, "observed revision one", &before_switch)
            .await
            .is_err(),
        "a write must not stage work under a deactivated revision"
    );

    assert_eq!(memories(&store).await, 0, "an aborted write leaves no row");
    assert!(Work::counts(&store).await.unwrap().is_empty());

    let current = EmbeddingTarget::observe(&store).await.unwrap();
    insert(&store, "observed revision two", &current)
        .await
        .expect("a fresh observation commits");
    assert_eq!(pending(&store, "embed.r2").await, 1);
}
