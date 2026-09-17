//! The rest of a memory's life, against a real node (spec 014 AC-1).
//!
//! Every test here drives the shipped code through a real hiqlite node and a
//! real write gate: a capture goes through `Gate::evaluate` and reaches the
//! store as an `Admitted`, and every assertion is made by reading the store
//! back rather than by inspecting what the code intended to write. There is
//! no test-only door into any of it, because a test-only door is a door.
//!
//! A test asserts; the lints that forbid a panic in a library are what a test
//! is made of, so they are relaxed here and nowhere else.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

mod common;

use aicortex_store::{
    Captured, Counters, Lifecycle, MAX_EXPIRY_BATCH, MemoryFilter, MemoryRepo, ScopeId,
    StatusFilter, fingerprint,
};
use aicortex_types::{
    DecisionRef, MediaDigest, MediaRef, MemoryBody, MemoryId, MemoryKind, Promotion, Status,
    TrustClass,
};
use rahi_store::{TxnBuilder, Value};
use rahi_types::UnixSeconds;

/// Run one capture through the merge-aware path and commit it.
async fn capture(node: &common::Node, memory: &aicortex_types::Memory) -> Captured {
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
    captured
}

/// How many rows one memory has in a table.
async fn rows_for(node: &common::Node, table: &str, id: MemoryId) -> u64 {
    common::count(
        node,
        &format!("SELECT COUNT(*) AS count FROM {table} WHERE memory_id = $1"),
        vec![Value::from(id.to_string())],
    )
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn staged_merge_and_supersession_cannot_restore_an_erased_body_or_its_sources() {
    let node = common::node().await;
    let scope = common::scope("alice");
    let memory = common::memory(&scope, "erase this body permanently", 1_700_000_000);
    capture(&node, &memory).await;
    let repeat = common::memory(&scope, "erase this body permanently", 1_700_000_100);
    let next = common::memory(&scope, "replacement", 1_700_000_200);
    let mut merge = TxnBuilder::new();
    Lifecycle::new()
        .capture(
            &node.handle(),
            &mut merge,
            &common::admit(&repeat),
            &common::work(&repeat),
        )
        .await
        .unwrap();
    let mut supersede = TxnBuilder::new();
    Lifecycle::new()
        .capture(
            &node.handle(),
            &mut supersede,
            &common::admit(&next),
            &common::work(&next),
        )
        .await
        .unwrap();
    Lifecycle::new()
        .supersede(
            &node.handle(),
            &mut supersede,
            &scope,
            memory.id,
            next.id,
            next.created,
        )
        .await
        .unwrap();
    aicortex_store::Eraser::new()
        .erase(
            &node.handle(),
            &node.ledger,
            &aicortex_store::Erasure::new(
                &scope,
                memory.id,
                &aicortex_store::Authority::of(scope.owner.clone()),
                UnixSeconds::new(1_700_100_000),
            ),
        )
        .await
        .unwrap();
    let work_before = common::count(&node, "SELECT COUNT(*) AS count FROM outbox", vec![]).await;
    for txn in [merge, supersede] {
        node.handle()
            .txn(txn.into_statements())
            .await
            .expect_err("a stale write rolls the entire batch back");
        let row = MemoryRepo::new()
            .get(&node.handle(), &scope, memory.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.status, Status::Erased);
        assert!(row.body.text.is_empty());
        assert_eq!(rows_for(&node, "memory_source", memory.id).await, 0);
        assert_eq!(
            common::count(&node, "SELECT COUNT(*) AS count FROM outbox", vec![]).await,
            work_before
        );
        assert_eq!(
            Counters::stats(&node.handle(), &scope).await.unwrap().total,
            1
        );
    }
    assert!(
        MemoryRepo::new()
            .get(&node.handle(), &scope, next.id)
            .await
            .unwrap()
            .is_none(),
        "the successor insert rolled back too"
    );
    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_supersessions_recheck_the_cycle_inside_the_committing_transaction() {
    let node = common::node().await;
    let scope = common::scope("alice");
    let first = common::memory(&scope, "first", 1_700_000_000);
    let second = common::memory(&scope, "second", 1_700_000_100);
    capture(&node, &first).await;
    capture(&node, &second).await;
    let mut forward = TxnBuilder::new();
    let mut backward = TxnBuilder::new();
    Lifecycle::new()
        .supersede(
            &node.handle(),
            &mut forward,
            &scope,
            first.id,
            second.id,
            second.created,
        )
        .await
        .unwrap();
    Lifecycle::new()
        .supersede(
            &node.handle(),
            &mut backward,
            &scope,
            second.id,
            first.id,
            second.created,
        )
        .await
        .unwrap();
    node.handle().txn(forward.into_statements()).await.unwrap();
    node.handle()
        .txn(backward.into_statements())
        .await
        .expect_err("the second commit would close a cycle");
    assert_eq!(
        Lifecycle::new()
            .successor(&node.handle(), &scope, first.id)
            .await
            .unwrap(),
        Some(second.id)
    );
    assert_eq!(
        Lifecycle::new()
            .successor(&node.handle(), &scope, second.id)
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        Counters::stats(&node.handle(), &scope).await.unwrap().total,
        2
    );
    node.shutdown().await;
}

// ---------------------------------------------------------------- B-1

#[test]
fn b1_the_fingerprint_separates_scopes_and_kinds() {
    let alice = common::scope("alice");
    let bob = common::scope("bob");

    // The same text is a different fingerprint in a different scope, which is
    // what stops one subject's capture from merging into another's.
    assert_ne!(
        fingerprint::of_text(&alice, MemoryKind::Fact, "the sky is blue"),
        fingerprint::of_text(&bob, MemoryKind::Fact, "the sky is blue"),
    );
    // And a different kind, which is what stops a fact and a task that read
    // alike from collapsing into one row.
    assert_ne!(
        fingerprint::of_text(&alice, MemoryKind::Fact, "call the dentist"),
        fingerprint::of_text(&alice, MemoryKind::Task, "call the dentist"),
    );
    // Normalization happens before the digest, so two spellings of the same
    // claim are one claim (013 B-8).
    assert_eq!(
        fingerprint::of_text(&alice, MemoryKind::Fact, "the sky is blue"),
        fingerprint::of_text(&alice, MemoryKind::Fact, "the sky is blue  \n"),
    );
    // Thirty-two bytes, rendered as sixty-four hex characters.
    assert_eq!(
        fingerprint::of_text(&alice, MemoryKind::Fact, "x").len(),
        64
    );
}

// ---------------------------------------------------------------- FR-001

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr001_a_repeat_merges_into_one_row_with_two_sources_and_two_uses() {
    let node = common::node().await;
    let alice = common::scope("alice");
    let bob = common::scope("bob");
    let text = "the standup moved to nine";

    let first = common::memory(&alice, text, 1_700_000_000);
    assert!(matches!(
        capture(&node, &first).await,
        Captured::Inserted(_)
    ));

    // The repeat carries its own id and a later time. It must not be used.
    let repeat = common::memory(&alice, text, 1_700_000_900);
    assert_ne!(repeat.id, first.id);
    let captured = capture(&node, &repeat).await;
    assert_eq!(
        captured,
        Captured::Merged(first.id),
        "a repeated capture must merge into the row that already holds the content"
    );
    assert!(captured.is_merge());

    // One row.
    let listing = MemoryRepo::new()
        .list(
            &node.handle(),
            &common_key(),
            &alice,
            &MemoryFilter::all(),
            50,
            None,
        )
        .await
        .expect("the listing reads");
    assert_eq!(listing.memories.len(), 1, "a merge must not insert a row");
    assert_eq!(listing.memories[0].id, first.id);

    // Two source rows, one per capture.
    assert_eq!(
        rows_for(&node, "memory_source", first.id).await,
        2,
        "each capture appends its own source row (B-2)"
    );
    // The provenance projection of the record stays at one row per memory,
    // which is 012 B-2's invariant and is not this spec's to move.
    assert_eq!(rows_for(&node, "provenance", first.id).await, 1);

    // Used twice, and updated moved forward to the repeat.
    let stored = MemoryRepo::new()
        .get(&node.handle(), &alice, first.id)
        .await
        .expect("the read succeeds")
        .expect("the row is there");
    assert_eq!(stored.importance.uses(), 2, "a merge counts as a use (B-2)");
    assert_eq!(stored.updated, UnixSeconds::new(1_700_000_900));
    assert_eq!(stored.created, UnixSeconds::new(1_700_000_000));
    assert_eq!(stored.status, Status::Active);

    // The candidate's own id was never used for anything.
    assert!(
        MemoryRepo::new()
            .get(&node.handle(), &alice, repeat.id)
            .await
            .expect("the read succeeds")
            .is_none()
    );

    // The same text in another scope is a separate memory.
    let elsewhere = common::memory(&bob, text, 1_700_001_000);
    assert!(matches!(
        capture(&node, &elsewhere).await,
        Captured::Inserted(_)
    ));
    assert_eq!(
        common::count(
            &node,
            "SELECT COUNT(*) AS count FROM memory WHERE scope_id = $1",
            vec![Value::from(&ScopeId::of(&bob))],
        )
        .await,
        1
    );

    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b2_a_merge_unions_the_title_and_the_media_the_repeat_brought() {
    let node = common::node().await;
    let alice = common::scope("alice");
    let text = "the invoice is paid";

    let mut bare = common::memory(&alice, text, 1_700_000_000);
    bare.body = MemoryBody::text(text);
    capture(&node, &bare).await;

    let digest = MediaDigest::parse(format!("sha256:{}", "cd".repeat(32))).expect("a legal digest");
    let mut richer = common::memory(&alice, text, 1_700_000_500);
    richer.body = MemoryBody::text(text)
        .with_title("Invoice 4471")
        .with_media(MediaRef {
            digest,
            media_type: "application/pdf".to_owned(),
            bytes: Some(2048),
        });
    // The title and the media are not part of the fingerprint, so this is the
    // same memory arriving with more said about it.
    assert!(capture(&node, &richer).await.is_merge());

    let stored = MemoryRepo::new()
        .get(&node.handle(), &alice, bare.id)
        .await
        .expect("the read succeeds")
        .expect("the row is there");
    assert_eq!(stored.body.title.as_deref(), Some("Invoice 4471"));
    assert_eq!(stored.body.media.len(), 1);
    assert_eq!(stored.body.text, text, "a merge never rewrites the body");

    // A third capture bringing the same media does not duplicate it.
    let again = {
        let mut again = common::memory(&alice, text, 1_700_000_900);
        again.body = richer.body.clone();
        again
    };
    assert!(capture(&node, &again).await.is_merge());
    let stored = MemoryRepo::new()
        .get(&node.handle(), &alice, bare.id)
        .await
        .expect("the read succeeds")
        .expect("the row is there");
    assert_eq!(
        stored.body.media.len(),
        1,
        "the union is a union, not a push"
    );
    assert_eq!(stored.importance.uses(), 3);

    node.shutdown().await;
}

// ---------------------------------------------------------------- FR-002

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr002_a_superseded_memory_leaves_retrieval_and_stays_readable_by_id() {
    let node = common::node().await;
    let alice = common::scope("alice");

    let old = common::memory(&alice, "the standup is at nine", 1_700_000_000);
    capture(&node, &old).await;

    // The new memory and the demotion of the old one are one transaction,
    // which is what B-3 requires: the successor never lands without the
    // supersession, and the supersession never lands without the successor.
    let new = common::memory(&alice, "the standup is at ten", 1_700_000_500);
    let admitted = common::admit(&new);
    let work = common::work(&new);
    let mut txn = TxnBuilder::new();
    let lifecycle = Lifecycle::new();
    lifecycle
        .capture(&node.handle(), &mut txn, &admitted, &work)
        .await
        .expect("the successor stages");
    lifecycle
        .supersede(
            &node.handle(),
            &mut txn,
            &alice,
            old.id,
            new.id,
            UnixSeconds::new(1_700_000_500),
        )
        .await
        .expect("the supersession stages");
    node.handle()
        .txn(txn.into_statements())
        .await
        .expect("the pair commits");

    // Absent from a retrieval result.
    let active = MemoryRepo::new()
        .list(
            &node.handle(),
            &common_key(),
            &alice,
            &MemoryFilter::active(),
            50,
            None,
        )
        .await
        .expect("the listing reads");
    assert_eq!(active.memories.len(), 1);
    assert_eq!(active.memories[0].id, new.id);

    // Present in a by-id read, retained for audit.
    let stored = MemoryRepo::new()
        .get(&node.handle(), &alice, old.id)
        .await
        .expect("the read succeeds")
        .expect("a superseded memory is still a row");
    assert_eq!(stored.status, Status::Superseded(new.id));
    assert!(!stored.is_retrievable());
    assert_eq!(
        stored.body.text, "the standup is at nine",
        "supersession is not an overwrite"
    );

    // Walkable in both directions.
    assert_eq!(
        lifecycle
            .successor(&node.handle(), &alice, old.id)
            .await
            .expect("the forward walk reads"),
        Some(new.id)
    );
    assert_eq!(
        lifecycle
            .predecessor(&node.handle(), &alice, new.id)
            .await
            .expect("the backward walk reads"),
        Some(old.id)
    );
    assert_eq!(
        lifecycle
            .successor(&node.handle(), &alice, new.id)
            .await
            .expect("the forward walk reads"),
        None,
        "the head of the chain has no successor"
    );

    // The counters moved with the status, rather than being recounted.
    let stats = Counters::stats(&node.handle(), &alice)
        .await
        .expect("the counters read");
    assert_eq!(stats.by_status.get(StatusFilter::Active.label()), Some(&1));
    assert_eq!(
        stats.by_status.get(StatusFilter::Superseded.label()),
        Some(&1)
    );

    node.shutdown().await;
}

// ---------------------------------------------------------------- FR-006

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr006_a_supersession_that_would_close_a_cycle_is_refused() {
    let node = common::node().await;
    let alice = common::scope("alice");
    let lifecycle = Lifecycle::new();

    let first = common::memory(&alice, "the first claim", 1_700_000_000);
    let second = common::memory(&alice, "the second claim", 1_700_000_100);
    capture(&node, &first).await;
    capture(&node, &second).await;

    // A memory cannot supersede itself.
    let mut txn = TxnBuilder::new();
    let error = lifecycle
        .supersede(
            &node.handle(),
            &mut txn,
            &alice,
            first.id,
            first.id,
            UnixSeconds::new(1_700_000_200),
        )
        .await
        .expect_err("a self-supersession is refused");
    assert!(
        error.message().contains("cannot supersede itself"),
        "{error}"
    );
    assert_eq!(txn.len(), 0, "a refusal stages nothing");

    // first -> second is fine.
    let mut txn = TxnBuilder::new();
    lifecycle
        .supersede(
            &node.handle(),
            &mut txn,
            &alice,
            first.id,
            second.id,
            UnixSeconds::new(1_700_000_200),
        )
        .await
        .expect("a fresh supersession stages");
    node.handle()
        .txn(txn.into_statements())
        .await
        .expect("it commits");

    // second -> first would close the cycle, and is refused.
    let mut txn = TxnBuilder::new();
    let error = lifecycle
        .supersede(
            &node.handle(),
            &mut txn,
            &alice,
            second.id,
            first.id,
            UnixSeconds::new(1_700_000_300),
        )
        .await
        .expect_err("a cycle is refused");
    assert!(
        error.message().contains("supersession cycle"),
        "the refusal must say what it refused: {error}"
    );
    assert_eq!(txn.len(), 0, "a refusal stages nothing");

    // The negative control: the refusal changed nothing.
    let stored = MemoryRepo::new()
        .get(&node.handle(), &alice, second.id)
        .await
        .expect("the read succeeds")
        .expect("the row is there");
    assert_eq!(
        stored.status,
        Status::Active,
        "a refused supersession must leave the row exactly as it was"
    );

    node.shutdown().await;
}

// ---------------------------------------------------------------- B-4

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b4_a_correction_names_what_it_corrects_supersedes_it_and_carries_its_actor() {
    let node = common::node().await;
    let alice = common::scope("alice");
    let lifecycle = Lifecycle::new();

    let wrong = common::memory(&alice, "the release shipped on tuesday", 1_700_000_000);
    capture(&node, &wrong).await;

    let correction = common::derived_memory(
        &alice,
        "the release shipped on wednesday",
        1_700_000_500,
        MemoryKind::Correction,
        vec![wrong.id],
    );
    let admitted = common::admit(&correction);
    let work = common::work(&correction);
    let mut txn = TxnBuilder::new();
    let captured = lifecycle
        .correct(&node.handle(), &mut txn, &admitted, wrong.id, &work)
        .await
        .expect("the correction stages");
    node.handle()
        .txn(txn.into_statements())
        .await
        .expect("the correction commits");
    assert_eq!(captured, Captured::Inserted(correction.id));

    let old = MemoryRepo::new()
        .get(&node.handle(), &alice, wrong.id)
        .await
        .expect("the read succeeds")
        .expect("the row is there");
    assert_eq!(old.status, Status::Superseded(correction.id));

    let new = MemoryRepo::new()
        .get(&node.handle(), &alice, correction.id)
        .await
        .expect("the read succeeds")
        .expect("the row is there");
    assert_eq!(new.kind, MemoryKind::Correction);
    assert!(
        new.provenance.derived_from.contains(&wrong.id),
        "a correction names what it corrects (B-4)"
    );
    assert!(new.actor.is_human(), "a correction carries its actor (B-4)");
    // The derivation row is what makes the link queryable rather than a parse
    // away, and is what erasure reaches (B-8).
    assert_eq!(rows_for(&node, "memory_derivation", correction.id).await, 1);

    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b4_a_correction_that_does_not_correct_is_refused() {
    let node = common::node().await;
    let alice = common::scope("alice");
    let lifecycle = Lifecycle::new();

    let wrong = common::memory(&alice, "the release shipped on tuesday", 1_700_000_000);
    capture(&node, &wrong).await;

    // Not a Correction at all: a second claim, not a correction of the first.
    let not_a_correction = common::derived_memory(
        &alice,
        "the release shipped on wednesday",
        1_700_000_500,
        MemoryKind::Fact,
        vec![wrong.id],
    );
    let admitted = common::admit(&not_a_correction);
    let work = common::work(&not_a_correction);
    let mut txn = TxnBuilder::new();
    let error = lifecycle
        .correct(&node.handle(), &mut txn, &admitted, wrong.id, &work)
        .await
        .expect_err("a fact is not a correction");
    assert!(
        error.message().contains("rather than a correction"),
        "{error}"
    );
    assert_eq!(txn.len(), 0);

    // A Correction that names nothing: it does not say what it corrects.
    let unnamed = common::memory_of_kind(
        &alice,
        "actually it was wednesday",
        1_700_000_600,
        MemoryKind::Correction,
    );
    let admitted = common::admit(&unnamed);
    let work = common::work(&unnamed);
    let mut txn = TxnBuilder::new();
    let error = lifecycle
        .correct(&node.handle(), &mut txn, &admitted, wrong.id, &work)
        .await
        .expect_err("a correction that names nothing is refused");
    assert!(error.message().contains("does not name"), "{error}");
    assert_eq!(txn.len(), 0);

    // An agent's correction cannot be instruction grade. `Promotion` takes a
    // human subject, so the record type alone does not close this: the
    // promotion here is a real human's, attached to a memory an agent wrote.
    let mut by_agent = common::derived_memory(
        &alice,
        "actually it was thursday",
        1_700_000_700,
        MemoryKind::Correction,
        vec![wrong.id],
    );
    by_agent.actor = aicortex_types::Actor::agent(
        aicortex_types::ActorId::new("agent:summarizer").expect("a legal actor id"),
        aicortex_types::AgentOrigin::default(),
    );
    by_agent.trust = TrustClass::instruction(Promotion::new(
        common::sub("alice"),
        DecisionRef::new("01JC5X6Q0000000000000000").expect("a legal decision id"),
    ));
    assert!(by_agent.trust.is_actionable());
    let admitted = common::admit(&by_agent);
    let work = common::work(&by_agent);
    let mut txn = TxnBuilder::new();
    let error = lifecycle
        .correct(&node.handle(), &mut txn, &admitted, wrong.id, &work)
        .await
        .expect_err("an agent cannot make an instruction-grade correction");
    assert!(
        error.message().contains("cannot be instruction grade"),
        "{error}"
    );
    assert_eq!(txn.len(), 0);

    // The negative control: after three refusals the original is untouched.
    let stored = MemoryRepo::new()
        .get(&node.handle(), &alice, wrong.id)
        .await
        .expect("the read succeeds")
        .expect("the row is there");
    assert_eq!(stored.status, Status::Active);
    assert_eq!(
        common::count(
            &node,
            "SELECT COUNT(*) AS count FROM memory WHERE scope_id = $1",
            vec![Value::from(&ScopeId::of(&alice))],
        )
        .await,
        1,
        "a refused correction stores nothing"
    );

    node.shutdown().await;
}

// ---------------------------------------------------------------- B-5

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b5_expiry_moves_due_rows_in_bounded_batches_and_never_deletes() {
    let node = common::node().await;
    let alice = common::scope("alice");
    let lifecycle = Lifecycle::new();
    let now = UnixSeconds::new(1_700_100_000);

    // Three memories that were true until yesterday, and one that carries no
    // deadline at all.
    let mut due = Vec::new();
    for index in 0..3_u64 {
        let memory = common::memory(&alice, &format!("the badge expires {index}"), 1_700_000_000);
        capture(&node, &memory).await;
        let mut txn = TxnBuilder::new();
        Lifecycle::set_valid_until(
            &mut txn,
            &alice,
            memory.id,
            Some(UnixSeconds::new(1_700_090_000)),
        );
        node.handle()
            .txn(txn.into_statements())
            .await
            .expect("the deadline commits");
        due.push(memory);
    }
    let permanent = common::memory(&alice, "the office is on the third floor", 1_700_000_000);
    capture(&node, &permanent).await;

    // A batch is bounded, and the caller is told there is more.
    let first = lifecycle
        .expire_due(&node.handle(), &alice, now, 2)
        .await
        .expect("the first batch runs");
    assert_eq!(first.memories, 2);
    assert!(first.more, "two of three due leaves more to do");

    let second = lifecycle
        .expire_due(&node.handle(), &alice, now, 2)
        .await
        .expect("the second batch runs");
    assert_eq!(second.memories, 1);
    assert!(!second.more);

    let third = lifecycle
        .expire_due(&node.handle(), &alice, now, 2)
        .await
        .expect("the third batch runs");
    assert_eq!(
        third.memories, 0,
        "an idempotent pass expires nothing twice"
    );

    // Expiry is a status change, never a delete: every row is still there and
    // still says what it said.
    for memory in &due {
        let stored = MemoryRepo::new()
            .get(&node.handle(), &alice, memory.id)
            .await
            .expect("the read succeeds")
            .expect("an expired memory is still a row");
        assert_eq!(stored.status, Status::Expired);
        assert_eq!(stored.body.text, memory.body.text);
        assert!(!stored.is_retrievable());
    }
    let untouched = MemoryRepo::new()
        .get(&node.handle(), &alice, permanent.id)
        .await
        .expect("the read succeeds")
        .expect("the row is there");
    assert_eq!(
        untouched.status,
        Status::Active,
        "a memory with no deadline is not swept"
    );

    // The record and the column agree, which is what the json_set rewrite is
    // for: a listing filtered on the column returns exactly the rows whose
    // record says the same thing.
    let expired = MemoryRepo::new()
        .list(
            &node.handle(),
            &common_key(),
            &alice,
            &MemoryFilter {
                status: Some(StatusFilter::Expired),
                kind: None,
            },
            50,
            None,
        )
        .await
        .expect("the listing reads");
    assert_eq!(expired.memories.len(), 3);
    assert!(
        expired
            .memories
            .iter()
            .all(|memory| memory.status == Status::Expired)
    );

    // The counters moved in the same transaction as the status.
    let stats = Counters::stats(&node.handle(), &alice)
        .await
        .expect("the counters read");
    assert_eq!(stats.total, 4);
    assert_eq!(stats.by_status.get(StatusFilter::Active.label()), Some(&1));
    assert_eq!(stats.by_status.get(StatusFilter::Expired.label()), Some(&3));

    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b5_an_unbounded_expiry_batch_is_refused() {
    let node = common::node().await;
    let alice = common::scope("alice");
    let lifecycle = Lifecycle::new();
    let now = UnixSeconds::new(1_700_100_000);

    for batch in [0, MAX_EXPIRY_BATCH + 1] {
        let error = lifecycle
            .expire_due(&node.handle(), &alice, now, batch)
            .await
            .expect_err("an out-of-range batch is refused");
        assert!(error.message().contains("is not between 1 and"), "{error}");
    }
    // The bound itself is accepted, so the refusal is off-by-one correct.
    lifecycle
        .expire_due(&node.handle(), &alice, now, MAX_EXPIRY_BATCH)
        .await
        .expect("the largest legal batch runs");

    node.shutdown().await;
}

// ---------------------------------------------------------------- B-6

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b6_no_pass_rewrites_a_row_to_decay_it() {
    let node = common::node().await;
    let alice = common::scope("alice");
    let lifecycle = Lifecycle::new();

    let memory = common::memory(&alice, "the kitchen code is on the fridge", 1_700_000_000);
    capture(&node, &memory).await;
    let before = MemoryRepo::new()
        .get(&node.handle(), &alice, memory.id)
        .await
        .expect("the read succeeds")
        .expect("the row is there");

    // A year later, with every pass this module has run against the scope.
    let much_later = UnixSeconds::new(1_700_000_000 + 365 * 24 * 60 * 60);
    lifecycle
        .expire_due(&node.handle(), &alice, much_later, 100)
        .await
        .expect("the expiry pass runs");

    let after = MemoryRepo::new()
        .get(&node.handle(), &alice, memory.id)
        .await
        .expect("the read succeeds")
        .expect("the row is there");
    assert_eq!(
        after.importance, before.importance,
        "no pass here rewrites a row to decay it (B-6)"
    );
    assert_eq!(after.updated, before.updated);

    // The decay is real; it is just computed rather than stored.
    assert!(
        after.importance.decayed_at(much_later) < after.importance.base(),
        "importance decays by computation at read time (011 B-9)"
    );

    node.shutdown().await;
}

// ---------------------------------------------------------------- 012 D-4

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn d4_the_backfill_redigests_a_column_written_under_the_old_function() {
    let node = common::node().await;
    let alice = common::scope("alice");
    let lifecycle = Lifecycle::new();

    let memory = common::memory(&alice, "the lease renews in march", 1_700_000_000);
    capture(&node, &memory).await;
    let current = fingerprint::of_memory(&memory);

    // Rewrite the column to what 012's function produced before 014 landed:
    // SHA-256 over the kind and the body text, with no scope mixed in.
    let stale = "0".repeat(64);
    node.handle()
        .txn(vec![rahi_store::Statement::with_params(
            "UPDATE memory SET fingerprint = $1 WHERE id = $2",
            vec![Value::from(&*stale), Value::from(memory.id.to_string())],
        )])
        .await
        .expect("the stale column commits");
    assert!(
        MemoryRepo::new()
            .fingerprint_holder(&node.handle(), &alice, &current)
            .await
            .expect("the read succeeds")
            .is_none(),
        "a stale column means a repeat would duplicate rather than merge"
    );

    let changed = lifecycle
        .redigest(&node.handle(), &alice, 100)
        .await
        .expect("the backfill runs");
    assert_eq!(changed, 1);
    assert_eq!(
        MemoryRepo::new()
            .fingerprint_holder(&node.handle(), &alice, &current)
            .await
            .expect("the read succeeds"),
        Some(memory.id),
        "after the backfill the column is the digest of B-1"
    );

    // And the merge of B-2 works again, which is the point of the backfill.
    let repeat = common::memory(&alice, "the lease renews in march", 1_700_000_900);
    assert!(capture(&node, &repeat).await.is_merge());

    node.shutdown().await;
}

/// The cursor signing key a listing needs (012 B-7, D-5).
fn common_key() -> aicortex_store::CursorKey {
    aicortex_store::CursorKey::new([3u8; aicortex_store::CursorKey::BYTES])
}
