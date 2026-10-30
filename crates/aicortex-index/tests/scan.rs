//! The vector channel against a real store (spec 016 FR-001, FR-003,
//! FR-006, FR-007, B-4, B-9).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]
// Spec 010 B-1: this test crate belongs to aicortex-index, whose crate roots
// carry the one documented float allow; the naive reference sorts scores.
#![allow(clippy::float_arithmetic)]

mod common;

use std::path::Path;
use std::time::{Duration, Instant};

use aicortex_index::{
    Candidate, DEFAULT_BUDGET, Filter, ScanConfig, ScanIndex, ScanQuery, VectorIndex, dot,
    normalize, scan,
};
use aicortex_store::ScopeId;
use aicortex_types::{MemoryId, Status};
use common::{Chunk, Node};
use rahi_store::Value;

const DIMS: u16 = 384;

/// The naive reference: score every chunk, sort everything, take `k`.
fn reference(chunks: &[Chunk], query: &[f32], k: usize) -> Vec<(String, f64)> {
    // The scan normalizes its query; so does the reference.
    let mut needle = query.to_vec();
    normalize(&mut needle).unwrap();
    let query = needle.as_slice();
    let mut scored: Vec<(String, f64)> = chunks
        .iter()
        .map(|chunk| (chunk.chunk_id.clone(), f64::from(dot(query, &chunk.vector))))
        .collect();
    scored.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    scored.truncate(k);
    scored
}

/// A query wide enough to return every row it can see.
fn wide<'a>(
    scope: &'a ScopeId,
    revision: &'a common::ModelRevision,
    vector: &'a [f32],
    filter: &'a Filter,
) -> ScanQuery<'a> {
    ScanQuery {
        scope,
        model: revision.comparable(),
        vector,
        k: 500,
        filter,
    }
}

fn ids(candidates: &[Candidate]) -> Vec<(String, f64)> {
    candidates
        .iter()
        .map(|candidate| (candidate.chunk_id.clone(), candidate.score))
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr001_fr007_a_20000_chunk_scan_matches_the_reference_within_budget() {
    let node = Node::migrated().await;
    let store = node.handle();
    let (scope, scope_id) = common::scope("fr001");
    let model = common::revision(1, DIMS);
    common::register_scope(&store, &scope).await;
    common::register_model(&store, &model).await;
    let chunks = common::chunks(&scope_id, &model, 1, 5_000, 4, 7);
    assert_eq!(chunks.len(), 20_000);
    common::seed(&store, &scope_id, &model, &chunks, Status::Active).await;

    let filter = Filter::retrievable();
    for query_seed in [11_u64, 12, 13] {
        let needle = common::unit_vector(query_seed, DIMS);
        let query = ScanQuery {
            scope: &scope_id,
            model: model.comparable(),
            vector: &needle,
            k: 20,
            filter: &filter,
        };
        let started = Instant::now();
        let outcome = scan(&store, &query, ScanConfig::default()).await.unwrap();
        let elapsed = started.elapsed();

        // FR-001: the same top 20, in the same order, with the same scores.
        assert_eq!(ids(&outcome.candidates), reference(&chunks, &needle, 20));
        assert!(!outcome.truncated);
        assert_eq!(outcome.scanned, 20_000);
        // FR-007: the 20000 case is inside the budget on this machine.
        assert!(
            elapsed < DEFAULT_BUDGET,
            "a 20000-chunk scan took {elapsed:?}, over the {DEFAULT_BUDGET:?} budget"
        );
    }

    // The seam returns what the function returns.
    let needle = common::unit_vector(11, DIMS);
    let query = ScanQuery {
        scope: &scope_id,
        model: model.comparable(),
        vector: &needle,
        k: 20,
        filter: &filter,
    };
    let index = ScanIndex::new(store.clone(), ScanConfig::default());
    let seam = index.search(&query).await.unwrap();
    assert_eq!(ids(&seam.candidates), reference(&chunks, &needle, 20));
    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr003_a_scan_never_leaves_its_scope_or_its_revision() {
    let node = Node::migrated().await;
    let store = node.handle();
    let (scope_a, a) = common::scope("scope-a");
    let (scope_b, b) = common::scope("scope-b");
    let first = common::revision(1, 16);
    let second = common::revision(2, 16);
    common::register_scope(&store, &scope_a).await;
    common::register_scope(&store, &scope_b).await;
    common::register_model(&store, &first).await;
    common::register_model(&store, &second).await;

    // The same memories at both revisions in A, and other memories in B
    // whose vectors are copies of A's, so only the predicates separate them.
    let a_first = common::chunks(&a, &first, 1, 50, 2, 3);
    let a_second = common::chunks(&a, &second, 1, 50, 2, 3);
    let b_first: Vec<Chunk> = common::chunks(&b, &first, 1_000, 50, 2, 3)
        .into_iter()
        .zip(&a_first)
        .map(|(mut chunk, twin)| {
            chunk.vector.clone_from(&twin.vector);
            chunk
        })
        .collect();
    common::seed(&store, &a, &first, &a_first, Status::Active).await;
    common::seed(&store, &a, &second, &a_second, Status::Active).await;
    common::seed(&store, &b, &first, &b_first, Status::Active).await;

    let filter = Filter::retrievable();
    let needle = a_first[0].vector.clone();
    let at = |scope, revision| wide(scope, revision, &needle, &filter);

    let found = scan(&store, &at(&a, &first), ScanConfig::default())
        .await
        .unwrap();
    assert_eq!(found.candidates.len(), a_first.len());
    assert!(
        found
            .candidates
            .iter()
            .all(|c| c.chunk_id.ends_with(&format!("-1-{:04}", c.ordinal))
                && c.chunk_id.starts_with(&a.as_str()[..12]))
    );

    let found = scan(&store, &at(&a, &second), ScanConfig::default())
        .await
        .unwrap();
    assert_eq!(found.candidates.len(), a_second.len());
    let second_ids: Vec<&str> = a_second.iter().map(|c| c.chunk_id.as_str()).collect();
    assert!(
        found
            .candidates
            .iter()
            .all(|c| second_ids.contains(&c.chunk_id.as_str()))
    );

    let found = scan(&store, &at(&b, &second), ScanConfig::default())
        .await
        .unwrap();
    assert!(found.candidates.is_empty(), "B holds nothing at revision 2");

    // 015 D-26's predicate: a row at the right revision under another model
    // identity is not comparable and is not returned.
    store
        .execute(
            "UPDATE embedding SET model_id = 'another-model'
             WHERE scope_id = $1 AND model_revision = 2 AND memory_id = $2",
            vec![
                Value::from(a.as_str()),
                Value::from(common::memory_id(1).to_string()),
            ],
        )
        .await
        .unwrap();
    let found = scan(&store, &at(&a, &second), ScanConfig::default())
        .await
        .unwrap();
    assert_eq!(found.candidates.len(), a_second.len() - 2);
    assert!(
        found
            .candidates
            .iter()
            .all(|c| c.memory_id != common::memory_id(1))
    );
    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_status_filter_is_a_predicate_and_quarantine_is_invisible_by_default() {
    let node = Node::migrated().await;
    let store = node.handle();
    let (scope, scope_id) = common::scope("status");
    let model = common::revision(1, 16);
    common::register_scope(&store, &scope).await;
    common::register_model(&store, &model).await;
    let active = common::chunks(&scope_id, &model, 1, 10, 1, 5);
    let quarantined = common::chunks(&scope_id, &model, 100, 10, 1, 5);
    common::seed(&store, &scope_id, &model, &active, Status::Active).await;
    common::seed(&store, &scope_id, &model, &quarantined, Status::Quarantined).await;

    let needle = quarantined[0].vector.clone();
    let retrievable = Filter::retrievable();
    let query = ScanQuery {
        scope: &scope_id,
        model: model.comparable(),
        vector: &needle,
        k: 100,
        filter: &retrievable,
    };
    let found = scan(&store, &query, ScanConfig::default()).await.unwrap();
    assert_eq!(found.candidates.len(), 10);
    assert_eq!(found.scanned, 10, "quarantined rows are never read");
    let quarantined_ids: Vec<MemoryId> = quarantined.iter().map(|c| c.memory).collect();
    assert!(
        found
            .candidates
            .iter()
            .all(|c| !quarantined_ids.contains(&c.memory_id))
    );

    let widened = Filter::statuses([Status::Active, Status::Quarantined]).unwrap();
    let query = ScanQuery {
        filter: &widened,
        ..query
    };
    let found = scan(&store, &query, ScanConfig::default()).await.unwrap();
    assert_eq!(found.candidates.len(), 20);
    assert_eq!(found.candidates[0].memory_id, quarantined[0].memory);
    assert!(Filter::statuses([]).is_err());
    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr006_an_exhausted_budget_returns_the_best_so_far_marked_truncated() {
    let node = Node::migrated().await;
    let store = node.handle();
    let (scope, scope_id) = common::scope("budget");
    let model = common::revision(1, 16);
    common::register_scope(&store, &scope).await;
    common::register_model(&store, &model).await;
    let chunks = common::chunks(&scope_id, &model, 1, 100, 1, 9);
    common::seed(&store, &scope_id, &model, &chunks, Status::Active).await;

    let filter = Filter::retrievable();
    let needle = common::unit_vector(1, 16);
    let query = ScanQuery {
        scope: &scope_id,
        model: model.comparable(),
        vector: &needle,
        k: 5,
        filter: &filter,
    };
    let config = ScanConfig {
        page_size: 10,
        budget: Duration::ZERO,
    };
    let outcome = scan(&store, &query, config).await.unwrap();
    assert!(outcome.truncated);
    assert_eq!(outcome.candidates.len(), 5);
    assert_eq!(outcome.pages, 1);
    assert_eq!(outcome.scanned, 10);

    // With the budget restored the same scan reads every page and is whole,
    // including when the last page is exactly full.
    let outcome = scan(
        &store,
        &query,
        ScanConfig {
            page_size: 10,
            budget: Duration::from_secs(60),
        },
    )
    .await
    .unwrap();
    assert!(!outcome.truncated);
    assert_eq!(outcome.scanned, 100);
    // The empty read past an exactly full last page is not a page.
    assert_eq!(outcome.pages, 10);
    assert_eq!(ids(&outcome.candidates), reference(&chunks, &needle, 5));
    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ties_break_by_chunk_id_and_a_wrong_width_query_is_refused() {
    let node = Node::migrated().await;
    let store = node.handle();
    let (scope, scope_id) = common::scope("ties");
    let model = common::revision(1, 8);
    common::register_scope(&store, &scope).await;
    common::register_model(&store, &model).await;
    // Every chunk carries the same vector, so only the tie-break orders them.
    let same = common::unit_vector(42, 8);
    let chunks: Vec<Chunk> = common::chunks(&scope_id, &model, 1, 30, 1, 1)
        .into_iter()
        .rev()
        .map(|mut chunk| {
            chunk.vector.clone_from(&same);
            chunk
        })
        .collect();
    common::seed(&store, &scope_id, &model, &chunks, Status::Active).await;

    let filter = Filter::retrievable();
    let query = ScanQuery {
        scope: &scope_id,
        model: model.comparable(),
        vector: &same,
        k: 7,
        filter: &filter,
    };
    let config = ScanConfig {
        page_size: 4,
        ..ScanConfig::default()
    };
    let first = scan(&store, &query, config).await.unwrap();
    let mut expected: Vec<String> = chunks.iter().map(|c| c.chunk_id.clone()).collect();
    expected.sort();
    expected.truncate(7);
    let got: Vec<String> = first
        .candidates
        .iter()
        .map(|c| c.chunk_id.clone())
        .collect();
    assert_eq!(got, expected);
    assert_eq!(scan(&store, &query, config).await.unwrap(), first);

    let short = [1.0_f32; 4];
    let error = scan(
        &store,
        &ScanQuery {
            vector: &short,
            ..query
        },
        config,
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("dimensions"), "{error}");
    let zero = [0.0_f32; 8];
    assert!(
        scan(
            &store,
            &ScanQuery {
                vector: &zero,
                ..query
            },
            config
        )
        .await
        .is_err()
    );
    let none = scan(&store, &ScanQuery { k: 0, ..query }, config)
        .await
        .unwrap();
    assert!(none.candidates.is_empty());
    node.shutdown().await;
}

/// Every Rust source and manifest under the workspace's crates and apps.
fn sources(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            if path.file_name().is_some_and(|name| name == "target") {
                continue;
            }
            sources(&path, out);
        } else if path
            .extension()
            .is_some_and(|ext| ext == "rs" || ext == "toml")
        {
            out.push(path);
        }
    }
}

#[test]
fn b9_no_extension_and_no_custom_function_is_registered_anywhere() {
    // Spelled in pieces so this file does not match its own search.
    let needles = [
        concat!("load", "_extension"),
        concat!("create", "_scalar_function"),
        concat!("create", "_aggregate_function"),
        concat!("create", "_window_function"),
        concat!("create", "_collation"),
    ];
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut files = Vec::new();
    for dir in ["crates", "apps"] {
        sources(&root.join(dir), &mut files);
    }
    sources(&root.join("fixtures"), &mut files);
    files.push(root.join("Cargo.toml"));
    assert!(files.len() > 20, "the walk found the workspace sources");
    for file in files {
        let text = std::fs::read_to_string(&file).unwrap();
        for needle in needles {
            assert!(
                !text.contains(needle),
                "{} names `{needle}`: no extension and no custom SQL function (B-9)",
                file.display()
            );
        }
    }
}
