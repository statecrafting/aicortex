//! The repositories, checked (spec 012).
//!
//! - B-4, FR-002: a capture is one transaction, and a failure on its last
//!   statement leaves no trace of the first.
//! - B-3, B-9, FR-003: a read for scope A never returns a row of scope B,
//!   over a fixture with two scopes and identical content, including the
//!   derivation rows.
//! - B-7, FR-004: 5000 rows page exactly once each, with a concurrent insert
//!   landing mid-listing.
//! - B-7, FR-005: a cursor is bound to its scope and its filter, and a
//!   tampered one is refused.
//! - B-6, FR-006: `stats` is one statement whose plan touches only the
//!   counter table.
//! - B-8: a body over the ceiling is refused at the storage boundary.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

mod common;

use std::collections::HashSet;

use aicortex_store::{
    Counters, Cursor, CursorKey, MemoryFilter, MemoryRepo, ProvenanceRepo, ScopeId, ScopeRepo,
    StatusFilter,
};
use aicortex_types::{ExtractorVersion, MemoryId, MemoryKind};
use rahi_store::{Statement, TxnBuilder, Value};
use rahi_types::{Error, UnixSeconds};
use serde::Deserialize;

fn key() -> CursorKey {
    CursorKey::new([9u8; CursorKey::BYTES])
}

#[derive(Debug, Deserialize)]
struct Count {
    count: i64,
}

#[derive(Debug, Deserialize)]
struct Plan {
    detail: String,
}

/// How many rows a table holds, over every scope: a test-only read, which is
/// why it lives here and not in the crate (B-3 governs the crate's statements
/// and this one proves a row is absent everywhere).
async fn rows(store: &rahi_store::StoreHandle, table: &str) -> i64 {
    let counted: Vec<Count> = store
        .query_consistent(format!("SELECT count(*) AS count FROM {table}"), vec![])
        .await
        .unwrap();
    counted.first().map(|row| row.count).unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b4_fr002_a_capture_commits_whole_or_leaves_nothing() {
    let fixture = common::Fixture::migrated().await;
    let store = fixture.handle();
    let repo = MemoryRepo::new();
    let scope = common::scope("atomic");

    // Two parents, so the capture under test carries two derivation rows.
    let mut parents = Vec::new();
    for (position, text) in ["parent one", "parent two"].into_iter().enumerate() {
        let parent = common::memory(&scope, text, 1_000 + position as u64);
        let mut txn = TxnBuilder::new();
        repo.insert(
            &mut txn,
            &parent,
            &parent.provenance,
            &common::work(&parent),
        )
        .unwrap();
        store.txn(txn.into_statements()).await.unwrap();
        parents.push(parent.id);
    }

    let mut derived = common::memory(&scope, "derived from both", 2_000);
    derived.provenance = derived.provenance.clone().derived(
        parents.clone(),
        ExtractorVersion::new("test-extractor", "1").unwrap(),
    );

    let before = (
        rows(&store, "memory").await,
        rows(&store, "provenance").await,
        rows(&store, "memory_derivation").await,
        rows(&store, "outbox").await,
    );

    // The whole capture, then one statement that cannot succeed. rahi's `txn`
    // is one Raft operation inside one SQLite transaction, so the failure
    // rolls back everything staged before it.
    let mut txn = TxnBuilder::new();
    repo.insert(
        &mut txn,
        &derived,
        &derived.provenance,
        &common::work(&derived),
    )
    .unwrap();
    let staged = txn.len();
    assert_eq!(
        staged, 7,
        "a capture with two parents stages scope, memory, provenance, two derivations, counter, outbox"
    );
    txn.push(Statement::with_params(
        "INSERT INTO memory (id) VALUES ($1)",
        vec![Value::from("this row has no scope and cannot land")],
    ));
    let outcome = store.txn(txn.into_statements()).await;
    assert!(outcome.is_err(), "the failing statement was accepted");

    assert_eq!(
        (
            rows(&store, "memory").await,
            rows(&store, "provenance").await,
            rows(&store, "memory_derivation").await,
            rows(&store, "outbox").await,
        ),
        before,
        "the rolled-back capture left a trace"
    );
    assert!(
        repo.get(&store, &scope, derived.id)
            .await
            .unwrap()
            .is_none(),
        "the rolled-back memory is readable"
    );

    // The same capture without the poisoned statement lands whole.
    let mut txn = TxnBuilder::new();
    repo.insert(
        &mut txn,
        &derived,
        &derived.provenance,
        &common::work(&derived),
    )
    .unwrap();
    store.txn(txn.into_statements()).await.unwrap();

    let stored = repo.get(&store, &scope, derived.id).await.unwrap().unwrap();
    assert_eq!(stored, derived, "the record did not round-trip");
    assert_eq!(
        ProvenanceRepo::get(&store, &scope, derived.id)
            .await
            .unwrap()
            .unwrap(),
        derived.provenance,
        "the provenance row did not round-trip"
    );
    assert_eq!(
        ProvenanceRepo::parents(&store, &scope, derived.id)
            .await
            .unwrap(),
        parents,
        "the derivation rows lost their order"
    );
    assert_eq!(rows(&store, "outbox").await, 3, "the outbox work is staged");

    fixture.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b3_b9_fr003_a_read_for_one_scope_never_returns_another() {
    let fixture = common::Fixture::migrated().await;
    let store = fixture.handle();
    let repo = MemoryRepo::new();
    let alice = common::scope("alice");
    let bob = common::scope("bob");

    // Identical content in both scopes: the fingerprint is the same, and the
    // unique index is per scope, so both land.
    let mut ids = Vec::new();
    for scope in [&alice, &bob] {
        let memory = common::memory(scope, "the same sentence in both scopes", 5_000);
        let mut txn = TxnBuilder::new();
        repo.insert(
            &mut txn,
            &memory,
            &memory.provenance,
            &common::work(&memory),
        )
        .unwrap();
        store.txn(txn.into_statements()).await.unwrap();
        ids.push(memory.id);
    }
    let (alice_id, bob_id) = (ids[0], ids[1]);
    assert_eq!(rows(&store, "memory").await, 2);

    assert!(
        repo.get(&store, &alice, bob_id).await.unwrap().is_none(),
        "alice read bob's memory by id"
    );
    assert!(
        repo.get(&store, &alice, alice_id).await.unwrap().is_some(),
        "alice cannot read her own memory"
    );

    let listing = repo
        .list(&store, &key(), &alice, &MemoryFilter::all(), 50, None)
        .await
        .unwrap();
    assert_eq!(listing.memories.len(), 1);
    assert_eq!(listing.memories[0].id, alice_id);

    assert!(
        ProvenanceRepo::get(&store, &alice, bob_id)
            .await
            .unwrap()
            .is_none(),
        "alice read bob's provenance"
    );

    // The same fingerprint resolves to each scope's own holder, never the
    // other's.
    let fingerprint =
        aicortex_store::fingerprint(&repo.get(&store, &alice, alice_id).await.unwrap().unwrap());
    assert_eq!(
        repo.fingerprint_holder(&store, &alice, &fingerprint)
            .await
            .unwrap(),
        Some(alice_id)
    );
    assert_eq!(
        repo.fingerprint_holder(&store, &bob, &fingerprint)
            .await
            .unwrap(),
        Some(bob_id)
    );

    assert_eq!(Counters::stats(&store, &alice).await.unwrap().total, 1);
    assert_eq!(Counters::stats(&store, &bob).await.unwrap().total, 1);

    assert_eq!(
        ScopeRepo::get(&store, &alice).await.unwrap().unwrap().scope,
        alice
    );

    // B-9: a derivation that names a parent in another scope is not a
    // relationship this system exposes. The row is written (nothing here
    // reaches across scopes to check it), and the read does not return it.
    let mut cross = common::memory(&alice, "derived across the boundary", 6_000);
    cross.provenance = cross.provenance.clone().derived(
        vec![bob_id, alice_id],
        ExtractorVersion::new("test-extractor", "1").unwrap(),
    );
    let mut txn = TxnBuilder::new();
    repo.insert(&mut txn, &cross, &cross.provenance, &common::work(&cross))
        .unwrap();
    store.txn(txn.into_statements()).await.unwrap();
    assert_eq!(
        ProvenanceRepo::parents(&store, &alice, cross.id)
            .await
            .unwrap(),
        vec![alice_id],
        "a parent in another scope was reachable through the derivation"
    );

    fixture.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b7_fr004_five_thousand_rows_page_exactly_once_each() {
    const ROWS: usize = 5_000;
    const PAGE: u32 = 250;

    let fixture = common::Fixture::migrated().await;
    let store = fixture.handle();
    let repo = MemoryRepo::new();
    let scope = common::scope("pager");

    // Ten rows share each `created`, so the `(created desc, id desc)` order
    // is exercised on its tiebreaker rather than only on its leading column.
    let mut expected = HashSet::with_capacity(ROWS);
    let mut txn = TxnBuilder::new();
    for index in 0..ROWS {
        let memory = common::memory(
            &scope,
            &format!("memory {index}"),
            100_000 + index as u64 / 10,
        );
        repo.insert(
            &mut txn,
            &memory,
            &memory.provenance,
            &common::work(&memory),
        )
        .unwrap();
        expected.insert(memory.id);
        if index % 500 == 499 {
            store
                .txn(std::mem::take(&mut txn).into_statements())
                .await
                .unwrap();
        }
    }
    assert!(txn.is_empty(), "every batch was submitted");
    assert_eq!(expected.len(), ROWS);

    let filter = MemoryFilter::active();
    let mut seen: Vec<MemoryId> = Vec::with_capacity(ROWS);
    let mut cursor: Option<String> = None;
    let mut pages = 0;
    loop {
        let listing = repo
            .list(&store, &key(), &scope, &filter, PAGE, cursor.as_deref())
            .await
            .unwrap();
        seen.extend(listing.memories.iter().map(|memory| memory.id));
        pages += 1;

        // The concurrent insert of FR-004: a row lands in the middle of the
        // range while the caller is three pages in.
        if pages == 3 {
            let late = common::memory(&scope, "landed while paging", 100_250);
            let mut txn = TxnBuilder::new();
            repo.insert(&mut txn, &late, &late.provenance, &common::work(&late))
                .unwrap();
            store.txn(txn.into_statements()).await.unwrap();
        }

        match listing.next {
            Some(next) => cursor = Some(next),
            None => break,
        }
        assert!(pages < 100, "the listing did not terminate");
    }

    let unique: HashSet<MemoryId> = seen.iter().copied().collect();
    assert_eq!(
        unique.len(),
        seen.len(),
        "a row was returned on more than one page"
    );
    let missing: Vec<&MemoryId> = expected.iter().filter(|id| !unique.contains(id)).collect();
    assert!(
        missing.is_empty(),
        "{} of the {ROWS} rows were never returned",
        missing.len()
    );

    // Ordering: newest first, ties broken by the id, descending.
    let ordered = repo
        .list(&store, &key(), &scope, &filter, 20, None)
        .await
        .unwrap();
    let keys: Vec<(u64, MemoryId)> = ordered
        .memories
        .iter()
        .map(|memory| (memory.created.get(), memory.id))
        .collect();
    let mut sorted = keys.clone();
    sorted.sort_by(|left, right| right.cmp(left));
    assert_eq!(keys, sorted, "the page is not in (created desc, id desc)");

    fixture.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b7_fr005_a_cursor_is_bound_to_its_scope_and_its_filter() {
    let fixture = common::Fixture::migrated().await;
    let store = fixture.handle();
    let repo = MemoryRepo::new();
    let alice = common::scope("cursor-alice");
    let bob = common::scope("cursor-bob");

    for scope in [&alice, &bob] {
        for index in 0..4u64 {
            let memory = common::memory_of_kind(
                scope,
                &format!("row {index}"),
                9_000 + index,
                MemoryKind::Fact,
            );
            let mut txn = TxnBuilder::new();
            repo.insert(
                &mut txn,
                &memory,
                &memory.provenance,
                &common::work(&memory),
            )
            .unwrap();
            store.txn(txn.into_statements()).await.unwrap();
        }
    }

    let filter_x = MemoryFilter::active();
    let filter_y = MemoryFilter::active().of_kind(MemoryKind::Fact);
    let page = repo
        .list(&store, &key(), &alice, &filter_x, 2, None)
        .await
        .unwrap();
    let cursor = page.next.expect("a full page continues");

    // It works where it was issued.
    let next = repo
        .list(&store, &key(), &alice, &filter_x, 2, Some(&cursor))
        .await
        .unwrap();
    assert_eq!(next.memories.len(), 2);

    // FR-005: not under another filter.
    let refused = repo
        .list(&store, &key(), &alice, &filter_y, 2, Some(&cursor))
        .await;
    assert!(
        matches!(refused, Err(Error::Validation(_))),
        "a cursor from filter X was accepted under filter Y: {refused:?}"
    );

    // Nor in another scope, even though the row ids would resolve there.
    let refused = repo
        .list(&store, &key(), &bob, &filter_x, 2, Some(&cursor))
        .await;
    assert!(
        matches!(refused, Err(Error::Validation(_))),
        "alice's cursor was accepted in bob's listing: {refused:?}"
    );

    // Nor tampered with: every single-character edit is refused.
    for position in 0..cursor.len() {
        let mut bytes = cursor.clone().into_bytes();
        bytes[position] = if bytes[position] == b'A' { b'B' } else { b'A' };
        let tampered = String::from_utf8(bytes).unwrap();
        if tampered == cursor {
            continue;
        }
        let refused = repo
            .list(&store, &key(), &alice, &filter_x, 2, Some(&tampered))
            .await;
        assert!(
            matches!(refused, Err(Error::Validation(_))),
            "a cursor edited at {position} was accepted"
        );
    }

    // Nor under another key: a cursor is not a bearer of its own authority.
    let other_key = CursorKey::new([1u8; CursorKey::BYTES]);
    let refused = repo
        .list(&store, &other_key, &alice, &filter_x, 2, Some(&cursor))
        .await;
    assert!(
        matches!(refused, Err(Error::Validation(_))),
        "a cursor signed by another key was accepted"
    );

    // And it round-trips the position it names.
    let scope_id = ScopeId::of(&alice);
    let decoded = Cursor::decode(&cursor, &key(), &scope_id, &filter_x).unwrap();
    assert_eq!(decoded.created, page.memories[1].created);
    assert_eq!(decoded.id, page.memories[1].id);
    assert_eq!(
        Cursor::new(decoded.created, decoded.id).encode(&key(), &scope_id, &filter_x),
        cursor,
        "encoding is not deterministic"
    );

    // There is no offset paging to fall back on.
    for limit in [0, aicortex_store::MAX_PAGE_ROWS + 1] {
        let refused = repo
            .list(&store, &key(), &alice, &filter_x, limit, None)
            .await;
        assert!(
            matches!(refused, Err(Error::Validation(_))),
            "a page of {limit} rows was accepted"
        );
    }

    fixture.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b6_fr006_stats_is_one_bounded_statement_over_the_counters() {
    let fixture = common::Fixture::migrated().await;
    let store = fixture.handle();
    let repo = MemoryRepo::new();
    let scope = common::scope("counted");

    for (index, kind) in [MemoryKind::Fact, MemoryKind::Task, MemoryKind::Fact]
        .into_iter()
        .enumerate()
    {
        let memory = common::memory_of_kind(
            &scope,
            &format!("counted {index}"),
            7_000 + index as u64,
            kind,
        );
        let mut txn = TxnBuilder::new();
        repo.insert(
            &mut txn,
            &memory,
            &memory.provenance,
            &common::work(&memory),
        )
        .unwrap();
        store.txn(txn.into_statements()).await.unwrap();
    }

    let stats = Counters::stats(&store, &scope).await.unwrap();
    assert_eq!(stats.total, 3);
    assert_eq!(stats.by_kind.get("fact"), Some(&2));
    assert_eq!(stats.by_kind.get("task"), Some(&1));
    assert_eq!(stats.by_status.get("active"), Some(&3));

    // FR-006, by statement count: `stats` runs exactly one statement.
    let statement = Counters::stats_statement(&scope);
    assert_eq!(
        statement.sql.matches(';').count(),
        0,
        "stats is more than one statement: {}",
        statement.sql
    );

    // FR-006, by plan: nothing in it touches the memory table, so its cost is
    // a property of the taxonomies rather than of how many memories exist.
    let plan: Vec<Plan> = store
        .query(
            format!("EXPLAIN QUERY PLAN {}", statement.sql),
            statement.params.clone(),
        )
        .await
        .unwrap();
    assert!(!plan.is_empty(), "the planner said nothing");
    for step in &plan {
        assert!(
            step.detail.contains("scope_counter"),
            "stats touches something other than the counters: {}",
            step.detail
        );
    }

    fixture.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b8_a_body_over_the_ceiling_is_refused_at_the_storage_boundary() {
    let fixture = common::Fixture::migrated().await;
    let store = fixture.handle();
    let scope = common::scope("oversized");

    let repo = MemoryRepo::new();
    assert_eq!(
        repo.max_body_bytes(),
        aicortex_store::DEFAULT_MAX_BODY_BYTES
    );

    let oversized = common::memory(
        &scope,
        &"x".repeat(aicortex_store::DEFAULT_MAX_BODY_BYTES + 1),
        8_000,
    );
    let mut txn = TxnBuilder::new();
    let refused = repo.insert(
        &mut txn,
        &oversized,
        &oversized.provenance,
        &common::work(&oversized),
    );
    assert!(
        matches!(refused, Err(Error::Validation(_))),
        "an oversized body was staged: {refused:?}"
    );
    assert!(
        txn.is_empty(),
        "a refused capture staged {} statements",
        txn.len()
    );

    // Exactly at the ceiling is admitted.
    let at_ceiling = common::memory(
        &scope,
        &"x".repeat(aicortex_store::DEFAULT_MAX_BODY_BYTES),
        8_001,
    );
    let mut txn = TxnBuilder::new();
    repo.insert(
        &mut txn,
        &at_ceiling,
        &at_ceiling.provenance,
        &common::work(&at_ceiling),
    )
    .unwrap();
    store.txn(txn.into_statements()).await.unwrap();

    // A lowered ceiling refuses what the default admitted (B-8: configured).
    let strict = MemoryRepo::new().with_max_body_bytes(16);
    let mut txn = TxnBuilder::new();
    let refused = strict.insert(
        &mut txn,
        &at_ceiling,
        &at_ceiling.provenance,
        &common::work(&at_ceiling),
    );
    assert!(matches!(refused, Err(Error::Validation(_))));

    // And the provenance offered must be the record's own (D-2).
    let other = common::memory(&scope, "another memory", 8_002);
    let mut txn = TxnBuilder::new();
    let refused = repo.insert(
        &mut txn,
        &at_ceiling,
        &other.provenance,
        &common::work(&at_ceiling),
    );
    assert!(
        matches!(refused, Err(Error::Validation(_))),
        "a memory was staged with somebody else's provenance"
    );

    fixture.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b2_the_fingerprint_index_refuses_a_duplicate_within_a_scope() {
    let fixture = common::Fixture::migrated().await;
    let store = fixture.handle();
    let repo = MemoryRepo::new();
    let scope = common::scope("duplicate");

    let first = common::memory(&scope, "one sentence, written twice", 9_500);
    let mut txn = TxnBuilder::new();
    repo.insert(&mut txn, &first, &first.provenance, &common::work(&first))
        .unwrap();
    store.txn(txn.into_statements()).await.unwrap();

    // The uniqueness read finds it through the leader before the write is
    // attempted (B-5), and the index refuses the write if a caller ignores
    // the answer.
    let fingerprint = aicortex_store::fingerprint(&first);
    assert_eq!(
        repo.fingerprint_holder(&store, &scope, &fingerprint)
            .await
            .unwrap(),
        Some(first.id)
    );

    let second = common::memory(&scope, "one sentence, written twice", 9_600);
    assert_ne!(second.id, first.id);
    let mut txn = TxnBuilder::new();
    repo.insert(
        &mut txn,
        &second,
        &second.provenance,
        &common::work(&second),
    )
    .unwrap();
    assert!(
        store.txn(txn.into_statements()).await.is_err(),
        "the unique index admitted a duplicate"
    );
    assert_eq!(rows(&store, "memory").await, 1);

    // A different status filter still finds the row it asks for.
    let listing = repo
        .list(
            &store,
            &key(),
            &scope,
            &MemoryFilter {
                status: Some(StatusFilter::Quarantined),
                kind: None,
            },
            10,
            None,
        )
        .await
        .unwrap();
    assert!(listing.memories.is_empty(), "nothing is quarantined here");
    assert!(listing.next.is_none());

    fixture.shutdown().await;
}

#[test]
fn a_scope_id_is_derived_and_distinguishes_every_shape() {
    let owner = common::sub("shapes");
    let personal = aicortex_types::Scope::personal(owner.clone());
    let project = aicortex_types::Scope::project(
        owner.clone(),
        aicortex_types::ProjectKey::new("apollo").unwrap(),
    );
    let shared = aicortex_types::Scope::shared(
        owner.clone(),
        aicortex_types::ShareKey::new("apollo").unwrap(),
    );

    let ids = [
        ScopeId::of(&personal),
        ScopeId::of(&project),
        ScopeId::of(&shared),
        ScopeId::of(&aicortex_types::Scope::personal(common::sub("other"))),
    ];
    let unique: HashSet<&str> = ids.iter().map(ScopeId::as_str).collect();
    assert_eq!(unique.len(), ids.len(), "two scopes share an id: {ids:?}");
    assert_eq!(
        ScopeId::of(&personal),
        ScopeId::of(&aicortex_types::Scope::personal(owner)),
        "the same scope derived two ids"
    );
}

#[test]
fn a_cursor_carries_nothing_a_reader_can_use() {
    let scope = ScopeId::of(&common::scope("opaque"));
    let filter = MemoryFilter::all();
    let id = MemoryId::now_v7();
    let cursor = Cursor::new(UnixSeconds::new(1_700_000_000), id).encode(&key(), &scope, &filter);

    assert!(
        !cursor.contains(&id.to_string()),
        "the cursor spells its id out"
    );
    assert!(
        !cursor.contains('=') && !cursor.contains('+') && !cursor.contains('/'),
        "the cursor is not base64url without padding: {cursor}"
    );
    assert!(
        Cursor::decode("not a cursor", &key(), &scope, &filter).is_err(),
        "an arbitrary string decoded"
    );
    assert!(
        Cursor::decode("", &key(), &scope, &filter).is_err(),
        "the empty string decoded"
    );
}
