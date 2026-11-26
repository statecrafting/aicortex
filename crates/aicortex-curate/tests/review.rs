//! The review queue and promotion (spec 023 FR-001 to FR-005, AC-1, AC-2).
//!
//! Every test runs against a real single-voter node and the chassis's real
//! decision chain. The HTTP half (the 403 of FR-003, the scopes, the
//! envelopes) is `aicortex-api`'s `tests/review.rs`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

mod common;

use aicortex_curate::{
    DEFAULT_REVIEW_INTERVAL_SECS, MAX_REASON_CHARS, MAX_REVIEW_INTERVAL_SECS, Promoter, Resolution,
    ReviewFilter, ReviewItem, ReviewKind, ReviewQueue, ReviewState, Reviewer,
};
use aicortex_store::MemoryRepo;
use aicortex_types::{Memory, MemoryId, Scope, Status, TrustClass};
use common::{Node, add_contradiction, add_edge, add_memory, count, edge, node, person, scope};
use rahi_store::Statement;
use rahi_types::{Error, UnixSeconds};

const T0: u64 = 1_800_000_000;

fn at(offset: u64) -> UnixSeconds {
    UnixSeconds::new(T0 + offset)
}

async fn read(node: &Node, scope: &Scope, id: MemoryId) -> Memory {
    MemoryRepo::new()
        .get(&node.handle(), scope, id)
        .await
        .expect("the memory reads")
        .expect("the memory exists")
}

async fn quarantined(node: &Node, scope: &Scope, text: &str) -> MemoryId {
    add_memory(
        &node.handle(),
        scope,
        text,
        Status::Quarantined,
        TrustClass::Evidence,
        T0,
    )
    .await
}

async fn active(node: &Node, scope: &Scope, text: &str) -> MemoryId {
    add_memory(
        &node.handle(),
        scope,
        text,
        Status::Active,
        TrustClass::Assertion,
        T0,
    )
    .await
}

/// The one open item of `kind` after a reconciliation.
async fn the_open_item(node: &Node, scope: &Scope, kind: ReviewKind) -> ReviewItem {
    let queue = ReviewQueue::new();
    queue.collect(&node.handle(), scope, at(1)).await.unwrap();
    let page = queue
        .list(
            &node.handle(),
            scope,
            ReviewFilter {
                kind: Some(kind),
                state: Some(ReviewState::Open),
            },
            None,
            None,
        )
        .await
        .unwrap();
    assert_eq!(page.items.len(), 1, "exactly one open {kind} item");
    page.items.into_iter().next().unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr001_a_quarantined_memory_is_admitted_as_an_assertion_and_never_higher() {
    let node = node().await;
    let scope = scope("alice");
    let memory = quarantined(&node, &scope, "origin unknown").await;
    // Not retrievable while it waits (014 B-6).
    assert!(!read(&node, &scope, memory).await.status.is_retrievable());

    let item = the_open_item(&node, &scope, ReviewKind::Quarantine).await;
    assert_eq!(item.subject, memory.to_string());
    let resolved = Reviewer::new(&node.handle(), &node.ledger)
        .resolve(
            &scope,
            &item.id,
            &person("alice"),
            &Resolution::Admit,
            at(10),
        )
        .await
        .unwrap();

    let admitted = read(&node, &scope, memory).await;
    assert!(admitted.status.is_retrievable());
    assert_eq!(admitted.trust, TrustClass::Assertion);
    assert_eq!(resolved.state, ReviewState::Resolved);
    assert_eq!(resolved.resolved_by, Some(person("alice")));
    assert_eq!(resolved.resolution.as_deref(), Some("admit"));
    assert_eq!(node.decisions("review.admit").await, 1);
    assert_eq!(
        resolved.decision_id.as_deref(),
        Some(format!("review.admit-{}", item.id).as_str())
    );
    // The counters moved with the row (012 B-6).
    assert_eq!(
        count(
            &node.handle(),
            &scope,
            "SELECT COALESCE(SUM(count), 0) AS count FROM scope_counter
             WHERE scope_id = ?1 AND status = 'quarantined'"
        )
        .await,
        0
    );
    node.ledger.verify().await.unwrap();
    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admission_stages_the_embedding_work_when_a_model_is_active() {
    let node = node().await;
    let scope = scope("alice");
    node.handle()
        .execute(
            "INSERT INTO embedding_model (model_id, revision, dims, normalized, first_seen, active)
             VALUES ('review-test', 1, 3, 1, 1700000000, 1)",
            vec![],
        )
        .await
        .expect("a model activates");
    let memory = quarantined(&node, &scope, "origin unknown").await;
    let item = the_open_item(&node, &scope, ReviewKind::Quarantine).await;

    Reviewer::new(&node.handle(), &node.ledger)
        .resolve(
            &scope,
            &item.id,
            &person("alice"),
            &Resolution::Admit,
            at(10),
        )
        .await
        .unwrap();

    assert!(read(&node, &scope, memory).await.status.is_retrievable());
    let scope_id = aicortex_store::ScopeId::of(&scope);
    let queue = aicortex_store::embedding_queue_counts(
        &node.handle(),
        scope_id.as_str(),
        aicortex_store::EMBEDDING_NAMESPACE,
        &format!("{}.r[0-9]*", aicortex_store::EMBEDDING_PROCESSOR),
        i64::try_from(aicortex_store::EMBEDDING_PROCESSOR.len() + 3).unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(queue.pending, 1, "the admitted memory is queued to embed");
    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ac2_rejecting_a_quarantined_memory_erases_it_and_the_chain_verifies() {
    let node = node().await;
    let scope = scope("alice");
    let memory = quarantined(&node, &scope, "origin unknown").await;
    let item = the_open_item(&node, &scope, ReviewKind::Quarantine).await;

    let resolved = Reviewer::new(&node.handle(), &node.ledger)
        .resolve(
            &scope,
            &item.id,
            &person("alice"),
            &Resolution::Reject,
            at(10),
        )
        .await
        .unwrap();

    assert_eq!(read(&node, &scope, memory).await.status, Status::Erased);
    assert_eq!(resolved.resolution.as_deref(), Some("reject"));
    assert!(resolved.decision_id.is_some());
    assert_eq!(node.decisions("memory.erase").await, 1);
    node.ledger.verify().await.unwrap();
    node.shutdown().await;
}

/// D-10: a rejection's erasure and the item's close commit together. When
/// the close cannot commit, nothing is erased and the item stays open, so the
/// reconciliation never withdraws it without a Decision; once the close can
/// commit, the item names the Decision 014 appended.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn d10_a_rejection_erases_and_closes_together_or_does_neither() {
    let node = node().await;
    let scope = scope("alice");
    let memory = quarantined(&node, &scope, "origin unknown").await;
    let item = the_open_item(&node, &scope, ReviewKind::Quarantine).await;
    let handle = node.handle();
    let reviewer = Reviewer::new(&handle, &node.ledger);
    // Any failure of the close statement stands for the second half failing.
    handle
        .txn(vec![Statement::new(
            "CREATE TRIGGER refuse_close BEFORE UPDATE ON review_item
             BEGIN SELECT RAISE(ABORT, 'the close is refused'); END",
        )])
        .await
        .unwrap();

    let refused = reviewer
        .resolve(
            &scope,
            &item.id,
            &person("alice"),
            &Resolution::Reject,
            at(10),
        )
        .await;

    assert!(refused.is_err(), "{refused:?}");
    assert_eq!(
        read(&node, &scope, memory).await.status,
        Status::Quarantined
    );
    let still = ReviewQueue::new()
        .get(&handle, &scope, &item.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(still.state, ReviewState::Open);
    assert_eq!(node.decisions("memory.erase").await, 0);

    handle
        .txn(vec![Statement::new("DROP TRIGGER refuse_close")])
        .await
        .unwrap();
    let resolved = reviewer
        .resolve(
            &scope,
            &item.id,
            &person("alice"),
            &Resolution::Reject,
            at(11),
        )
        .await
        .unwrap();

    assert_eq!(read(&node, &scope, memory).await.status, Status::Erased);
    assert_eq!(resolved.state, ReviewState::Resolved);
    let erase_decisions: Vec<String> = node
        .ledger
        .records()
        .await
        .unwrap()
        .iter()
        .map(|record| record.decision().unwrap())
        .filter(|decision| decision.kind.as_str() == "memory.erase")
        .map(|decision| decision.id.as_str().to_owned())
        .collect();
    assert_eq!(
        erase_decisions,
        vec![resolved.decision_id.clone().unwrap()],
        "the item names the one erasure Decision"
    );
    node.ledger.verify().await.unwrap();
    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_resolution_must_answer_the_items_kind_and_an_item_resolves_once() {
    let node = node().await;
    let scope = scope("alice");
    quarantined(&node, &scope, "origin unknown").await;
    let item = the_open_item(&node, &scope, ReviewKind::Quarantine).await;
    let handle = node.handle();
    let reviewer = Reviewer::new(&handle, &node.ledger);

    let wrong = reviewer
        .resolve(
            &scope,
            &item.id,
            &person("alice"),
            &Resolution::Decline,
            at(10),
        )
        .await;
    assert!(matches!(wrong, Err(Error::Validation(_))), "{wrong:?}");

    reviewer
        .resolve(
            &scope,
            &item.id,
            &person("alice"),
            &Resolution::Admit,
            at(11),
        )
        .await
        .unwrap();
    let again = reviewer
        .resolve(
            &scope,
            &item.id,
            &person("alice"),
            &Resolution::Admit,
            at(12),
        )
        .await;
    assert!(matches!(again, Err(Error::Conflict(_))), "{again:?}");
    assert_eq!(node.decisions("review.admit").await, 1);

    // Another owner's scope cannot see the item at all.
    let hidden = reviewer
        .resolve(
            &self::scope("mallory"),
            &item.id,
            &person("mallory"),
            &Resolution::Admit,
            at(13),
        )
        .await;
    assert!(matches!(hidden, Err(Error::NotFound(_))), "{hidden:?}");
    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr005_keeping_one_edge_leaves_exactly_one_active_edge() {
    let node = node().await;
    let scope = scope("alice");
    let source = active(&node, &scope, "alice works on two things").await;
    add_edge(&node.handle(), &scope, "edg-a", "apollo", 100, None, source).await;
    add_edge(&node.handle(), &scope, "edg-b", "gemini", 200, None, source).await;
    let contradiction = add_contradiction(&node.handle(), &scope, "edg-a", "edg-b").await;

    let item = the_open_item(&node, &scope, ReviewKind::Contradiction).await;
    assert_eq!(item.subject, contradiction);
    let evidence = ReviewQueue::new()
        .evidence(&node.handle(), &scope, &item)
        .await
        .unwrap();
    assert_eq!(evidence, vec![source], "the memories both edges came from");

    let handle = node.handle();
    let reviewer = Reviewer::new(&handle, &node.ledger);
    for (survivor, reason) in [
        ("edg-nope", "not one of the pair".to_owned()),
        ("edg-a", "   ".to_owned()),
        ("edg-a", "x".repeat(MAX_REASON_CHARS + 1)),
        ("edg-a", "a\nnewline".to_owned()),
    ] {
        let refused = reviewer
            .resolve(
                &scope,
                &item.id,
                &person("alice"),
                &Resolution::KeepEdge {
                    survivor: survivor.to_owned(),
                    reason,
                },
                at(10),
            )
            .await;
        assert!(matches!(refused, Err(Error::Validation(_))), "{refused:?}");
    }
    assert_eq!(node.decisions("review.contradiction").await, 0);

    let resolved = reviewer
        .resolve(
            &scope,
            &item.id,
            &person("alice"),
            &Resolution::KeepEdge {
                survivor: "edg-b".to_owned(),
                reason: "the newer note supersedes the older".to_owned(),
            },
            at(11),
        )
        .await
        .unwrap();

    assert_eq!(edge(&node.handle(), &scope, "edg-b").await.status, "active");
    let loser = edge(&node.handle(), &scope, "edg-a").await;
    assert_eq!(
        loser.status, "superseded",
        "nothing is deleted (017 FR-005)"
    );
    assert_eq!(loser.valid_until, Some(i64::try_from(T0 + 11).unwrap()));
    assert_eq!(
        count(
            &node.handle(),
            &scope,
            "SELECT COUNT(*) AS count FROM edge WHERE scope_id = ?1 AND status = 'active'"
        )
        .await,
        1
    );
    assert_eq!(
        resolved.reason.as_deref(),
        Some("the newer note supersedes the older")
    );
    assert_eq!(node.decisions("review.contradiction").await, 1);
    node.ledger.verify().await.unwrap();
    node.shutdown().await;
}

/// D-7: a losing edge that already ended before the resolution keeps its
/// end, and FR-003: an edge that stops being active before the resolution
/// commits aborts it, so the item stays open and no act is half-done.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn keep_edge_keeps_an_earlier_end_and_refuses_an_edge_no_longer_active() {
    let node = node().await;
    let scope = scope("alice");
    let source = active(&node, &scope, "alice works on two things").await;
    add_edge(
        &node.handle(),
        &scope,
        "edg-a",
        "apollo",
        100,
        Some(150),
        source,
    )
    .await;
    add_edge(&node.handle(), &scope, "edg-b", "gemini", 120, None, source).await;
    add_contradiction(&node.handle(), &scope, "edg-a", "edg-b").await;
    let item = the_open_item(&node, &scope, ReviewKind::Contradiction).await;
    let handle = node.handle();
    let reviewer = Reviewer::new(&handle, &node.ledger);
    let keep_b = Resolution::KeepEdge {
        survivor: "edg-b".to_owned(),
        reason: "the newer note supersedes the older".to_owned(),
    };
    reviewer
        .resolve(&scope, &item.id, &person("alice"), &keep_b, at(11))
        .await
        .unwrap();
    let loser = edge(&node.handle(), &scope, "edg-a").await;
    assert_eq!(loser.status, "superseded");
    assert_eq!(loser.valid_until, Some(150), "an earlier end is kept");

    add_edge(&node.handle(), &scope, "edg-c", "hermes", 300, None, source).await;
    add_edge(&node.handle(), &scope, "edg-d", "zeus", 400, None, source).await;
    add_contradiction(&node.handle(), &scope, "edg-c", "edg-d").await;
    let open = the_open_item(&node, &scope, ReviewKind::Contradiction).await;
    // Another writer retires the would-be loser before this resolution commits.
    node.handle()
        .txn(vec![rahi_store::Statement::with_params(
            "UPDATE edge SET status = 'superseded' WHERE scope_id = ?1 AND edge_id = 'edg-c'",
            vec![rahi_store::Value::from(&aicortex_store::ScopeId::of(
                &scope,
            ))],
        )])
        .await
        .unwrap();
    let refused = reviewer
        .resolve(
            &scope,
            &open.id,
            &person("alice"),
            &Resolution::KeepEdge {
                survivor: "edg-d".to_owned(),
                reason: "the later one holds".to_owned(),
            },
            at(13),
        )
        .await;
    assert!(refused.is_err(), "{refused:?}");
    let still = ReviewQueue::new()
        .get(&node.handle(), &scope, &open.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(still.state, ReviewState::Open, "the item is not closed");
    assert_eq!(
        edge(&node.handle(), &scope, "edg-c").await.valid_until,
        None
    );
    node.ledger.verify().await.unwrap();
    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr005_two_disjoint_periods_leave_two_active_edges_that_do_not_overlap() {
    let node = node().await;
    let scope = scope("alice");
    let source = active(&node, &scope, "alice changed projects").await;
    add_edge(&node.handle(), &scope, "edg-a", "apollo", 100, None, source).await;
    add_edge(&node.handle(), &scope, "edg-b", "gemini", 200, None, source).await;
    add_contradiction(&node.handle(), &scope, "edg-a", "edg-b").await;
    let item = the_open_item(&node, &scope, ReviewKind::Contradiction).await;
    let handle = node.handle();
    let reviewer = Reviewer::new(&handle, &node.ledger);

    // Before the later edge begins, the periods would still overlap.
    for boundary in [100, 150, 199] {
        let refused = reviewer
            .resolve(
                &scope,
                &item.id,
                &person("alice"),
                &Resolution::Disjoint {
                    boundary: UnixSeconds::new(boundary),
                    reason: "outside the shared period".to_owned(),
                },
                at(10),
            )
            .await;
        assert!(
            matches!(refused, Err(Error::Validation(_))),
            "boundary {boundary}: {refused:?}"
        );
    }

    reviewer
        .resolve(
            &scope,
            &item.id,
            &person("alice"),
            &Resolution::Disjoint {
                boundary: UnixSeconds::new(300),
                reason: "she moved projects at the start of the quarter".to_owned(),
            },
            at(11),
        )
        .await
        .unwrap();

    let first = edge(&node.handle(), &scope, "edg-a").await;
    let second = edge(&node.handle(), &scope, "edg-b").await;
    assert_eq!(
        (first.status.as_str(), second.status.as_str()),
        ("active", "active")
    );
    assert_eq!(first.valid_until, Some(300));
    assert_eq!(second.valid_from, 300);
    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr002_a_promotion_writes_one_decision_and_the_memory_carries_its_id() {
    let node = node().await;
    let scope = scope("alice");
    let memory = active(&node, &scope, "always run the linter").await;
    let alice = person("alice");

    let promoted = Promoter::new(&node.handle(), &node.ledger)
        .promote(&scope, memory, &alice, None, at(10))
        .await
        .unwrap();

    assert_eq!(node.decisions("memory.promote").await, 1);
    let stored = read(&node, &scope, memory).await;
    let promotion = stored.trust.promotion().expect("instruction grade");
    assert_eq!(promotion.decision().as_str(), promoted.decision);
    assert_eq!(promotion.by(), &alice);
    assert!(stored.trust.is_actionable());
    assert_eq!(
        promoted.review_due.get(),
        T0 + 10 + DEFAULT_REVIEW_INTERVAL_SECS,
        "D-1: a promotion expires by default"
    );
    // The memory's `trust` column agrees with its record.
    assert_eq!(
        count(
            &node.handle(),
            &scope,
            "SELECT COUNT(*) AS count FROM memory WHERE scope_id = ?1 AND trust = 'instruction'"
        )
        .await,
        1
    );
    // Promoting again is refused and appends nothing.
    let again = Promoter::new(&node.handle(), &node.ledger)
        .promote(&scope, memory, &alice, None, at(11))
        .await;
    assert!(matches!(again, Err(Error::Conflict(_))), "{again:?}");
    assert_eq!(node.decisions("memory.promote").await, 1);
    node.ledger.verify().await.unwrap();
    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_promotion_that_cannot_be_made_appends_no_decision() {
    let node = node().await;
    let scope = scope("alice");
    let alice = person("alice");
    let waiting = quarantined(&node, &scope, "origin unknown").await;
    let live = active(&node, &scope, "a rule").await;
    let handle = node.handle();
    let promoter = Promoter::new(&handle, &node.ledger);

    let held = promoter
        .promote(&scope, waiting, &alice, None, at(10))
        .await;
    assert!(
        matches!(held, Err(Error::Conflict(_))),
        "quarantined: {held:?}"
    );
    let absent = promoter
        .promote(&scope, MemoryId::now_v7(), &alice, None, at(10))
        .await;
    assert!(
        matches!(absent, Err(Error::NotFound(_))),
        "absent: {absent:?}"
    );
    for interval in [0, 59 * 60, MAX_REVIEW_INTERVAL_SECS + 1] {
        let bad = promoter
            .promote(&scope, live, &alice, Some(interval), at(10))
            .await;
        assert!(
            matches!(bad, Err(Error::Validation(_))),
            "{interval}: {bad:?}"
        );
    }
    // Another owner cannot promote in this scope.
    let foreign = promoter
        .promote(
            &self::scope("mallory"),
            live,
            &person("mallory"),
            None,
            at(10),
        )
        .await;
    assert!(
        matches!(foreign, Err(Error::NotFound(_))),
        "foreign: {foreign:?}"
    );

    assert_eq!(node.decisions("memory.promote").await, 0);
    assert_eq!(node.decisions("memory.promote_failed").await, 0);
    assert!(!read(&node, &scope, live).await.trust.is_actionable());
    node.shutdown().await;
}

/// FR-003: an approval commits only while its item is open. An item a
/// concurrent decline already closed aborts the promotion, and the memory
/// stays an assertion.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_approval_of_an_item_closed_meanwhile_promotes_nothing() {
    let node = node().await;
    let scope = scope("alice");
    let memory = active(&node, &scope, "approve me late").await;
    let queue = ReviewQueue::new();
    let request = queue
        .request_promotion(
            &node.handle(),
            &scope,
            memory,
            &person("agent-client"),
            at(1),
        )
        .await
        .unwrap();
    let handle = node.handle();
    let reviewer = Reviewer::new(&handle, &node.ledger);
    reviewer
        .resolve(
            &scope,
            &request.id,
            &person("bob"),
            &Resolution::Decline,
            at(5),
        )
        .await
        .unwrap();
    // The approval read the item as open before the decline committed.
    let late = Promoter::new(&handle, &node.ledger)
        .promote_for_item(&scope, memory, &person("alice"), None, at(6), &request.id)
        .await;
    assert!(late.is_err(), "{late:?}");
    assert!(!read(&node, &scope, memory).await.trust.is_actionable());
    let item = queue
        .get(&node.handle(), &scope, &request.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(item.state, ReviewState::Resolved);
    assert_eq!(item.resolution.as_deref(), Some("decline"));
    // An item id the scope does not hold promotes nothing either.
    let unknown = Promoter::new(&handle, &node.ledger)
        .promote_for_item(
            &scope,
            memory,
            &person("alice"),
            None,
            at(7),
            "no-such-item",
        )
        .await;
    assert!(unknown.is_err(), "{unknown:?}");
    assert!(!read(&node, &scope, memory).await.trust.is_actionable());
    node.ledger.verify().await.unwrap();
    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn approving_a_request_promotes_and_declining_leaves_the_memory_alone() {
    let node = node().await;
    let scope = scope("alice");
    let approved = active(&node, &scope, "approve me").await;
    let declined = active(&node, &scope, "decline me").await;
    let queue = ReviewQueue::new();
    let agent = person("agent-client");
    let first = queue
        .request_promotion(&node.handle(), &scope, approved, &agent, at(1))
        .await
        .unwrap();
    let repeated = queue
        .request_promotion(&node.handle(), &scope, approved, &agent, at(2))
        .await
        .unwrap();
    assert_eq!(first.id, repeated.id, "one open request per memory");
    assert_eq!(first.requested_by, Some(agent.clone()));
    let second = queue
        .request_promotion(&node.handle(), &scope, declined, &agent, at(3))
        .await
        .unwrap();
    assert_eq!(
        queue
            .open_count(&node.handle(), &scope, None)
            .await
            .unwrap(),
        2
    );

    let handle = node.handle();
    let reviewer = Reviewer::new(&handle, &node.ledger);
    let alice = person("alice");
    let done = reviewer
        .resolve(
            &scope,
            &first.id,
            &alice,
            &Resolution::Approve {
                interval_secs: Some(2 * 60 * 60),
            },
            at(10),
        )
        .await
        .unwrap();
    assert_eq!(done.state, ReviewState::Resolved);
    assert_eq!(done.resolved_by, Some(alice.clone()));
    assert!(read(&node, &scope, approved).await.trust.is_actionable());

    reviewer
        .resolve(&scope, &second.id, &alice, &Resolution::Decline, at(11))
        .await
        .unwrap();
    assert!(!read(&node, &scope, declined).await.trust.is_actionable());
    assert_eq!(node.decisions("memory.promote").await, 1);
    assert_eq!(node.decisions("review.decline").await, 1);
    assert_eq!(
        queue
            .open_count(&node.handle(), &scope, None)
            .await
            .unwrap(),
        0
    );

    // An instruction-grade memory, and a quarantined one, cannot be asked for.
    let held = quarantined(&node, &scope, "origin unknown").await;
    for memory in [approved, held] {
        let refused = queue
            .request_promotion(&node.handle(), &scope, memory, &agent, at(12))
            .await;
        assert!(matches!(refused, Err(Error::Conflict(_))), "{refused:?}");
    }
    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn demotion_returns_the_memory_to_assertion_and_the_ledger_keeps_both_records() {
    let node = node().await;
    let scope = scope("alice");
    let memory = active(&node, &scope, "a standing rule").await;
    let alice = person("alice");
    let handle = node.handle();
    let promoter = Promoter::new(&handle, &node.ledger);

    let not_yet = promoter.demote(&scope, memory, &alice, at(5)).await;
    assert!(matches!(not_yet, Err(Error::Conflict(_))), "{not_yet:?}");
    assert_eq!(node.decisions("memory.demote").await, 0);

    promoter
        .promote(&scope, memory, &alice, None, at(10))
        .await
        .unwrap();
    let demoted = promoter
        .demote(&scope, memory, &alice, at(20))
        .await
        .unwrap();

    assert_eq!(
        read(&node, &scope, memory).await.trust,
        TrustClass::Assertion
    );
    assert_eq!(demoted.memory, memory);
    assert_eq!(node.decisions("memory.promote").await, 1);
    assert_eq!(node.decisions("memory.demote").await, 1);
    assert_eq!(
        count(
            &node.handle(),
            &scope,
            "SELECT COUNT(*) AS count FROM promotion WHERE scope_id = ?1"
        )
        .await,
        0
    );
    // Promotion is reversible: it can be made again.
    promoter
        .promote(&scope, memory, &alice, None, at(30))
        .await
        .unwrap();
    node.ledger.verify().await.unwrap();
    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr004_a_lapsed_promotion_returns_to_assertion_and_opens_a_review_item() {
    let node = node().await;
    let scope = scope("alice");
    let memory = active(&node, &scope, "reconfirm me yearly").await;
    let alice = person("alice");
    let handle = node.handle();
    let promoter = Promoter::new(&handle, &node.ledger);
    promoter
        .promote(&scope, memory, &alice, Some(3_600), at(0))
        .await
        .unwrap();

    // The injected clock one second before the date: nothing lapses.
    let early = promoter.expire_due(&scope, at(3_599)).await.unwrap();
    assert!(early.is_empty());
    assert!(read(&node, &scope, memory).await.trust.is_actionable());

    let lapsed = promoter.expire_due(&scope, at(3_600)).await.unwrap();
    assert_eq!(lapsed.len(), 1);
    assert_eq!(lapsed[0].memory, memory);
    assert_eq!(
        read(&node, &scope, memory).await.trust,
        TrustClass::Assertion
    );
    assert_eq!(node.decisions("memory.promotion_lapsed").await, 1);

    let item = the_open_item(&node, &scope, ReviewKind::PromotionRequest).await;
    assert_eq!(item.subject, memory.to_string());
    assert_eq!(item.requested_by, None, "nobody asked; the lapse did");

    // A second sweep finds nothing, and appends nothing.
    let repeat = promoter.expire_due(&scope, at(7_200)).await.unwrap();
    assert!(repeat.is_empty());
    assert_eq!(node.decisions("memory.promotion_lapsed").await, 1);
    node.ledger.verify().await.unwrap();
    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_sweep_only_touches_its_own_scope() {
    let node = node().await;
    let alice_scope = scope("alice");
    let bob_scope = scope("bob");
    let theirs = active(&node, &bob_scope, "bob's rule").await;
    Promoter::new(&node.handle(), &node.ledger)
        .promote(&bob_scope, theirs, &person("bob"), Some(3_600), at(0))
        .await
        .unwrap();
    let swept = Promoter::new(&node.handle(), &node.ledger)
        .expire_due(&alice_scope, at(10_000))
        .await
        .unwrap();
    assert!(swept.is_empty());
    assert!(read(&node, &bob_scope, theirs).await.trust.is_actionable());
    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_queue_pages_filters_counts_and_withdraws_what_no_longer_waits() {
    let node = node().await;
    let scope = scope("alice");
    let other = self::scope("bob");
    let first = quarantined(&node, &scope, "first").await;
    quarantined(&node, &scope, "second").await;
    quarantined(&node, &scope, "third").await;
    quarantined(&node, &other, "someone else's").await;
    let queue = ReviewQueue::new();

    let collected = queue.collect(&node.handle(), &scope, at(1)).await.unwrap();
    assert_eq!((collected.opened, collected.withdrawn), (3, 0));
    // A second pass is idempotent.
    let again = queue.collect(&node.handle(), &scope, at(2)).await.unwrap();
    assert_eq!((again.opened, again.withdrawn), (0, 0));
    assert_eq!(
        queue
            .open_count(&node.handle(), &scope, None)
            .await
            .unwrap(),
        3
    );
    assert_eq!(
        queue
            .open_count(&node.handle(), &scope, Some(ReviewKind::Contradiction))
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        queue
            .open_count(&node.handle(), &other, None)
            .await
            .unwrap(),
        0,
        "another scope's memory is not collected into this scope's queue"
    );

    let filter = ReviewFilter {
        kind: Some(ReviewKind::Quarantine),
        state: Some(ReviewState::Open),
    };
    let page_one = queue
        .list(&node.handle(), &scope, filter, Some(2), None)
        .await
        .unwrap();
    assert_eq!(page_one.items.len(), 2);
    let cursor = page_one.next.clone().expect("a third item follows");
    let page_two = queue
        .list(&node.handle(), &scope, filter, Some(2), Some(&cursor))
        .await
        .unwrap();
    assert_eq!(page_two.items.len(), 1);
    assert!(page_two.next.is_none());
    let mut ids: Vec<_> = page_one
        .items
        .iter()
        .chain(&page_two.items)
        .map(|item| item.id.clone())
        .collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 3, "no item repeats across pages");

    for bad in ["", "nope", "-1.rvw-x", "5.other"] {
        let refused = queue
            .list(&node.handle(), &scope, filter, None, Some(bad))
            .await;
        assert!(
            matches!(refused, Err(Error::Validation(_))),
            "{bad:?}: {refused:?}"
        );
    }
    let zero = queue
        .list(&node.handle(), &scope, filter, Some(0), None)
        .await;
    assert!(matches!(zero, Err(Error::Validation(_))), "{zero:?}");

    // A memory erased elsewhere stops waiting: its item is withdrawn.
    let authority = aicortex_store::Authority::of(person("alice"));
    aicortex_store::Eraser::new()
        .erase(
            &node.handle(),
            &node.ledger,
            &aicortex_store::Erasure::new(&scope, first, &authority, at(5)),
        )
        .await
        .unwrap();
    let swept = queue.collect(&node.handle(), &scope, at(6)).await.unwrap();
    assert_eq!((swept.opened, swept.withdrawn), (0, 1));
    assert_eq!(
        queue
            .open_count(&node.handle(), &scope, None)
            .await
            .unwrap(),
        2
    );
    let withdrawn = queue
        .list(
            &node.handle(),
            &scope,
            ReviewFilter {
                kind: None,
                state: Some(ReviewState::Withdrawn),
            },
            None,
            None,
        )
        .await
        .unwrap();
    assert_eq!(withdrawn.items.len(), 1);
    assert_eq!(withdrawn.items[0].subject, first.to_string());
    node.shutdown().await;
}
