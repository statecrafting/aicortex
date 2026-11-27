//! The runner, against a real node (spec 030 B-2, B-3, B-4, B-8, B-10, and
//! FR-001 to FR-006).
//!
//! Every test drives the shipped runner through a real hiqlite node, the real
//! write gate and the real decision chain, over a stub source with a fixture
//! of items, and asserts by reading the store back.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

mod common;

use std::time::{Duration, Instant};

use aicortex_gate::{KIND_QUARANTINE, KIND_REFUSE};
use aicortex_ingest::{
    CancelOutcome, IngestPolicy, ItemRepo, PublicPolicy, RawItem, RunCounts, RunFailure, RunId,
    RunLimits, RunState, SourceId, SourceKind,
};
use aicortex_store::{Authority, Eraser, Erasure};
use common::{Faults, Stub, fixture, node, quick, request, runner, scope, text};
use rahi_store::Value;
use rahi_types::{Error, Sub, UnixSeconds};

fn texts(range: std::ops::Range<usize>) -> Vec<String> {
    let mut texts: Vec<_> = range.map(text).collect();
    texts.sort();
    texts
}

fn source(name: &str) -> SourceId {
    SourceId::new(name).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_thousand_items_import_a_thousand_memories_and_a_rerun_updates_them() {
    // FR-001, B-4.
    let node = node().await;
    let alice = scope("alice");
    let stub = Stub::new("notes", SourceKind::Authenticated, fixture(1_000));
    let runner = runner(&[stub], IngestPolicy::default());
    let runtime = node.runtime();

    let first = runner
        .run(&runtime, request(&alice, "notes"))
        .await
        .expect("the run starts");
    assert_eq!(first.state, RunState::Completed, "{first:?}");
    assert_eq!(first.failure, None);
    assert_eq!(
        first.counts,
        RunCounts {
            seen: 1_000,
            admitted: 1_000,
            ..RunCounts::default()
        }
    );
    assert_eq!(node.memory_rows(&alice).await, 1_000);
    assert_eq!(node.item_rows(&alice).await, 1_000);
    assert_eq!(node.bodies(&alice, "active").await, texts(0..1_000));
    assert_eq!(
        node.cursor(&alice, "notes").await,
        None,
        "a run that reached the end leaves no cursor, so the next one starts over"
    );

    let second = runner
        .run(&runtime, request(&alice, "notes"))
        .await
        .expect("the re-run starts");
    assert_eq!(second.state, RunState::Completed, "{second:?}");
    assert_eq!(
        second.counts,
        RunCounts {
            seen: 1_000,
            updated: 1_000,
            ..RunCounts::default()
        },
        "a re-run imports nothing and updates every item"
    );
    assert_eq!(node.memory_rows(&alice).await, 1_000, "no duplicate");
    assert_eq!(node.item_rows(&alice).await, 1_000);

    // The report is the row, and it is read inside the run's scope only.
    let stored = runner
        .report(
            &node.handle(),
            &alice,
            &source("notes"),
            &RunId::parse(&second.run_id).unwrap(),
        )
        .await
        .unwrap()
        .expect("the run is recorded");
    assert_eq!(stored, second);
    let elsewhere = runner
        .report(
            &node.handle(),
            &scope("mallory"),
            &source("notes"),
            &RunId::parse(&second.run_id).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(elsewhere, None, "another scope does not see the run");
    let wrong_source = runner
        .report(
            &node.handle(),
            &alice,
            &source("other"),
            &RunId::parse(&second.run_id).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(wrong_source, None);

    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_crash_mid_run_resumes_with_no_duplicate_and_no_skipped_item() {
    // FR-002, B-3: the process dies between pages, and the next run picks up
    // at the cursor that committed with the last page.
    let node = node().await;
    let alice = scope("alice");
    let stub = Stub::new("notes", SourceKind::Authenticated, fixture(1_000));
    stub.set_faults(Faults {
        park_at: Some(3),
        park_forever: true,
        ..Faults::default()
    });
    let runner = runner(std::slice::from_ref(&stub), IngestPolicy::default())
        .with_stale_after(Duration::ZERO);
    let runtime = node.runtime();

    let task = tokio::spawn({
        let (runner, runtime) = (runner.clone(), runtime.clone());
        async move {
            runner
                .run(&runtime, request(&scope("alice"), "notes"))
                .await
        }
    });
    stub.reached.notified().await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());

    // What the dead run left: three pages and the cursor that covers them.
    assert_eq!(node.memory_rows(&alice).await, 300);
    assert_eq!(node.cursor(&alice, "notes").await.as_deref(), Some("300"));
    assert_eq!(
        node.count(
            "SELECT COUNT(*) AS count FROM ingest_run WHERE state = 'running'",
            vec![]
        )
        .await,
        1,
        "its row still says running"
    );

    stub.set_faults(Faults::default());
    let started = runner
        .start(&runtime, request(&alice, "notes"))
        .await
        .expect("a silent run is taken over");
    assert!(started.resumes(), "the new run starts at the saved cursor");
    let report = runner.drive(&runtime, started).await;
    assert_eq!(report.state, RunState::Completed, "{report:?}");
    assert_eq!(
        report.counts,
        RunCounts {
            seen: 700,
            admitted: 700,
            ..RunCounts::default()
        },
        "the pages before the crash are not fetched again"
    );

    // Content, not just counts: exactly the fixture, each item once.
    assert_eq!(node.memory_rows(&alice).await, 1_000);
    assert_eq!(node.item_rows(&alice).await, 1_000);
    assert_eq!(node.bodies(&alice, "active").await, texts(0..1_000));
    assert_eq!(
        node.count(
            "SELECT COUNT(*) AS count FROM ingest_run
             WHERE state = 'failed' AND error_code = 'abandoned'",
            vec![]
        )
        .await,
        1,
        "the dead run is marked abandoned by the run that took its place"
    );
    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_run_that_cannot_progress_stops_with_a_code_and_leaves_the_cursor() {
    // B-3: past the retry ceiling the run stops, records why, and leaves the
    // cursor where the last committed page put it.
    let node = node().await;
    let alice = scope("alice");
    let stub = Stub::new("notes", SourceKind::Authenticated, fixture(500));
    stub.set_faults(Faults {
        transient_at: Some(2),
        ..Faults::default()
    });
    let runner = runner(std::slice::from_ref(&stub), IngestPolicy::default());
    let runtime = node.runtime();

    let report = runner
        .run(&runtime, request(&alice, "notes"))
        .await
        .unwrap();
    assert_eq!(report.state, RunState::Failed);
    assert_eq!(report.failure, Some(RunFailure::RetriesExhausted));
    assert_eq!(
        stub.served.load(std::sync::atomic::Ordering::SeqCst),
        2 + 3,
        "two pages, then the failing one asked for once and retried twice"
    );
    assert_eq!(node.cursor(&alice, "notes").await.as_deref(), Some("200"));
    assert_eq!(node.memory_rows(&alice).await, 200);

    stub.set_faults(Faults::default());
    let resumed = runner
        .run(&runtime, request(&alice, "notes"))
        .await
        .unwrap();
    assert_eq!(resumed.state, RunState::Completed, "{resumed:?}");
    assert_eq!(resumed.counts.admitted, 300);
    assert_eq!(node.bodies(&alice, "active").await, texts(0..500));
    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failure_that_is_not_worth_retrying_is_not_retried() {
    let node = node().await;
    let alice = scope("alice");
    let stub = Stub::new("notes", SourceKind::Authenticated, fixture(300));
    stub.set_faults(Faults {
        fatal_at: Some(1),
        ..Faults::default()
    });
    let runner = runner(std::slice::from_ref(&stub), IngestPolicy::default());

    let report = runner
        .run(&node.runtime(), request(&alice, "notes"))
        .await
        .unwrap();
    assert_eq!(report.state, RunState::Failed);
    assert_eq!(report.failure, Some(RunFailure::AdapterFailed));
    assert_eq!(stub.served.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert_eq!(node.cursor(&alice, "notes").await.as_deref(), Some("100"));
    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_page_the_runner_did_not_ask_for_is_refused_whole() {
    // Everything an adapter returns is untrusted: its size, its ids, and its
    // cursor are held to what the runner asked for before any of it is used.
    let node = node().await;
    let alice = scope("alice");
    let runtime = node.runtime();

    let oversized = Stub::new("big", SourceKind::Authenticated, fixture(300));
    oversized.set_faults(Faults {
        oversized_at: Some(1),
        ..Faults::default()
    });
    let stuck = Stub::new("stuck", SourceKind::Authenticated, fixture(300));
    stuck.set_faults(Faults {
        stuck_at: Some(1),
        ..Faults::default()
    });
    let mut malformed_items = fixture(300);
    malformed_items[150] = RawItem::new("", UnixSeconds::new(1), serde_json::json!({"text": "x"}));
    let malformed = Stub::new("malformed", SourceKind::Authenticated, malformed_items);
    // An adapter that never ends and never delivers: pages of nothing, each
    // with a cursor of its own, are bounded.
    let endless = Stub::new("endless", SourceKind::Authenticated, fixture(300));
    endless.set_faults(Faults {
        empty_from: Some(0),
        ..Faults::default()
    });
    let runner = runner(
        &[oversized, stuck, malformed, endless.clone()],
        IngestPolicy::default(),
    );

    let report = runner
        .run(&runtime, request(&alice, "endless"))
        .await
        .unwrap();
    assert_eq!(report.failure, Some(RunFailure::BadPage), "{report:?}");
    assert_eq!(
        endless.served.load(std::sync::atomic::Ordering::SeqCst),
        17,
        "the sixteenth empty page in a row is the last one accepted"
    );

    for (name, kept) in [("big", 100), ("stuck", 100), ("malformed", 100)] {
        let report = runner.run(&runtime, request(&alice, name)).await.unwrap();
        assert_eq!(report.state, RunState::Failed, "{name}");
        assert_eq!(report.failure, Some(RunFailure::BadPage), "{name}");
        assert_eq!(
            node.count(
                "SELECT COUNT(*) AS count FROM ingest_item WHERE scope_id = $1 AND source_id = $2",
                vec![
                    Value::from(&aicortex_store::ScopeId::of(&alice)),
                    Value::from(name)
                ]
            )
            .await,
            kept,
            "{name}: the bad page stored nothing"
        );
    }
    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dry_run_writes_nothing_and_reports_what_the_real_run_then_does() {
    // FR-003, B-10. The fixture has one of everything the report counts.
    let node = node().await;
    let alice = scope("alice");
    let secret = concat!("gh", "p_", "9wQk2LmXr4Tb8Zc1Nd6Vf3Hs5Jg7Pa0Ye2Uu");
    let mut items = fixture(10);
    let at = UnixSeconds::new(1_700_001_000);
    items.push(RawItem::new(
        "ext-dup",
        at,
        serde_json::json!({ "text": text(0) }),
    ));
    items.push(RawItem::new(
        "ext-bare",
        at,
        serde_json::json!({ "no_text": 1 }),
    ));
    items.push(RawItem::new(
        "ext-leak",
        at,
        serde_json::json!({ "text": format!("the deploy key is {secret}") }),
    ));
    items.push(RawItem::new(
        "ext-0003",
        at,
        serde_json::json!({ "text": "the meeting about subject 3 was cancelled" }),
    ));
    items.push(RawItem::new(
        "ext-0004",
        at,
        serde_json::json!({ "text": text(4) }),
    ));
    let stub = Stub::new("notes", SourceKind::Authenticated, items);
    let runner = runner(&[stub], IngestPolicy::default());
    let runtime = node.runtime();

    let mut dry = request(&alice, "notes");
    dry.dry_run = true;
    let started = runner.start(&runtime, dry).await.unwrap();
    assert!(started.is_dry_run());
    let dry_id = RunId::parse(started.run_id().as_str()).unwrap();
    let preview = runner.drive(&runtime, started).await;
    assert_eq!(preview.state, RunState::Completed, "{preview:?}");
    assert!(preview.dry_run);
    assert_eq!(
        preview.counts,
        RunCounts {
            seen: 15,
            admitted: 10,
            updated: 2,
            merged: 1,
            quarantined: 0,
            refused: 1,
            unmapped: 1,
        }
    );

    // Nothing was written, anywhere: not a memory, a mapping, a cursor, a run
    // row, a digest key, or a Decision.
    assert_eq!(node.memory_rows(&alice).await, 0);
    assert_eq!(node.item_rows(&alice).await, 0);
    assert_eq!(node.cursor(&alice, "notes").await, None);
    for (table, sql) in [
        ("ingest_run", "SELECT COUNT(*) AS count FROM ingest_run"),
        ("decision_key", "SELECT COUNT(*) AS count FROM decision_key"),
        ("decision_ref", "SELECT COUNT(*) AS count FROM decision_ref"),
        ("outbox", "SELECT COUNT(*) AS count FROM outbox"),
    ] {
        assert_eq!(node.count(sql, vec![]).await, 0, "{table}");
    }
    assert_eq!(node.decisions(KIND_REFUSE).await, 0);

    // The preview is still there to read, on the node that ran it.
    let remembered = runner
        .report(&node.handle(), &alice, &source("notes"), &dry_id)
        .await
        .unwrap();
    assert_eq!(remembered, Some(preview.clone()));

    let real = runner
        .run(&runtime, request(&alice, "notes"))
        .await
        .unwrap();
    assert_eq!(real.state, RunState::Completed);
    assert_eq!(
        real.counts, preview.counts,
        "the real run did what was shown"
    );
    assert_eq!(node.decisions(KIND_REFUSE).await, 1);
    assert_eq!(
        node.count("SELECT COUNT(*) AS count FROM decision_key", vec![])
            .await,
        1,
        "the refusal's digest key is staged with its batch"
    );
    // The changed item superseded the memory it replaced; the unchanged
    // repeat and the duplicate content merged into what was already there.
    assert_eq!(node.bodies(&alice, "superseded").await, [text(3)]);
    let mut active = texts(0..10);
    active.retain(|body| *body != text(3));
    active.push("the meeting about subject 3 was cancelled".to_owned());
    active.sort();
    assert_eq!(node.bodies(&alice, "active").await, active);
    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_public_source_lands_quarantined_unless_the_deployment_says_otherwise() {
    // FR-005, B-7.
    let node = node().await;
    let runtime = node.runtime();
    let quarantine = scope("alice");
    let admit = scope("bob");

    let stub = Stub::new("pages", SourceKind::Public, fixture(5));
    let default = runner(std::slice::from_ref(&stub), IngestPolicy::default());
    let report = default
        .run(&runtime, request(&quarantine, "pages"))
        .await
        .unwrap();
    assert_eq!(report.counts.quarantined, 5, "{report:?}");
    assert_eq!(report.counts.admitted, 0);
    assert_eq!(node.bodies(&quarantine, "quarantined").await, texts(0..5));
    assert!(node.bodies(&quarantine, "active").await.is_empty());
    assert_eq!(node.decisions(KIND_QUARANTINE).await, 5);
    assert_eq!(
        node.count(
            "SELECT COUNT(*) AS count FROM decision_ref WHERE kind = 'quarantine'",
            vec![]
        )
        .await,
        5,
        "each quarantined memory's reference committed with its row"
    );

    let open = runner(
        &[stub],
        IngestPolicy {
            public: PublicPolicy::Admit,
        },
    );
    let report = open.run(&runtime, request(&admit, "pages")).await.unwrap();
    assert_eq!(report.counts.admitted, 5, "{report:?}");
    assert_eq!(node.bodies(&admit, "active").await, texts(0..5));
    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn each_kind_of_source_stores_what_its_trust_allows() {
    // B-7, B-5: the kind decides the trust class, and no adapter says so.
    let node = node().await;
    let runtime = node.runtime();
    let alice = scope("alice");
    let authenticated = Stub::new("account", SourceKind::Authenticated, fixture(3));
    let export = Stub::new("upload", SourceKind::Export, fixture(3));
    let runner = runner(&[authenticated, export], IngestPolicy::default());

    let account = runner
        .run(&runtime, request(&alice, "account"))
        .await
        .unwrap();
    let upload = runner
        .run(&runtime, request(&alice, "upload"))
        .await
        .unwrap();
    assert_eq!(account.counts.admitted, 3);
    assert_eq!(
        upload.counts,
        RunCounts {
            seen: 3,
            merged: 3,
            ..RunCounts::default()
        },
        "the same content from a second source merges (014 B-2)"
    );

    for memory in node.memories(&alice).await {
        assert_eq!(memory.trust.label(), "evidence");
        assert_eq!(memory.status.label(), "active");
        let provenance = &memory.provenance;
        assert!(provenance.source.system.as_str() == "account");
        let external = provenance.source.external_id.as_deref().unwrap();
        let index: usize = external.strip_prefix("ext-").unwrap().parse().unwrap();
        assert_eq!(memory.body.text, text(index));
        assert_eq!(
            provenance.captured_at,
            UnixSeconds::new(1_700_000_000 + index as u64),
            "the item's own timestamp"
        );
        assert_eq!(memory.actor.kind.label(), "import");
        assert_eq!(memory.actor.id.as_str(), account.run_id);
    }

    // An export on its own is an assertion.
    let other = scope("bob");
    runner
        .run(&runtime, request(&other, "upload"))
        .await
        .unwrap();
    for memory in node.memories(&other).await {
        assert_eq!(memory.trust.label(), "assertion");
    }
    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelling_stops_the_run_within_one_page_and_leaves_a_consistent_cursor() {
    // FR-006.
    let node = node().await;
    let alice = scope("alice");
    let stub = Stub::new("notes", SourceKind::Authenticated, fixture(1_000));
    stub.set_faults(Faults {
        park_at: Some(2),
        ..Faults::default()
    });
    let runner = runner(std::slice::from_ref(&stub), IngestPolicy::default());
    let runtime = node.runtime();

    let started = runner
        .start(&runtime, request(&alice, "notes"))
        .await
        .unwrap();
    let run = RunId::parse(started.run_id().as_str()).unwrap();
    let task = tokio::spawn({
        let (runner, runtime) = (runner.clone(), runtime.clone());
        async move { runner.drive(&runtime, started).await }
    });
    stub.reached.notified().await;

    let outcome = runner
        .cancel(&node.handle(), &alice, &source("notes"), &run)
        .await
        .unwrap();
    assert_eq!(outcome, CancelOutcome::Requested);
    stub.release.notify_one();

    let report = task.await.unwrap();
    assert_eq!(report.state, RunState::Cancelled, "{report:?}");
    assert!(report.cancel_requested);
    assert_eq!(
        report.counts.seen, 300,
        "the page in hand finished, and no further one began"
    );
    assert_eq!(node.memory_rows(&alice).await, 300);
    assert_eq!(node.cursor(&alice, "notes").await.as_deref(), Some("300"));

    // Cancelling again, or cancelling what is not there, says so.
    assert_eq!(
        runner
            .cancel(&node.handle(), &alice, &source("notes"), &run)
            .await
            .unwrap(),
        CancelOutcome::Ended(RunState::Cancelled)
    );
    assert_eq!(
        runner
            .cancel(&node.handle(), &scope("mallory"), &source("notes"), &run)
            .await
            .unwrap(),
        CancelOutcome::Unknown,
        "a run of another scope cannot be cancelled"
    );

    // The cursor is where a resumed run needs it.
    stub.set_faults(Faults::default());
    let resumed = runner
        .run(&runtime, request(&alice, "notes"))
        .await
        .unwrap();
    assert_eq!(resumed.state, RunState::Completed);
    assert_eq!(resumed.counts.seen, 700);
    assert_eq!(node.bodies(&alice, "active").await, texts(0..1_000));
    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dry_run_is_cancelled_through_the_same_call() {
    let node = node().await;
    let alice = scope("alice");
    let stub = Stub::new("notes", SourceKind::Authenticated, fixture(1_000));
    stub.set_faults(Faults {
        park_at: Some(1),
        ..Faults::default()
    });
    let runner = runner(std::slice::from_ref(&stub), IngestPolicy::default());
    let runtime = node.runtime();

    let mut dry = request(&alice, "notes");
    dry.dry_run = true;
    let started = runner.start(&runtime, dry).await.unwrap();
    let run = RunId::parse(started.run_id().as_str()).unwrap();
    let task = tokio::spawn({
        let (runner, runtime) = (runner.clone(), runtime.clone());
        async move { runner.drive(&runtime, started).await }
    });
    stub.reached.notified().await;
    assert_eq!(
        runner
            .cancel(&node.handle(), &alice, &source("notes"), &run)
            .await
            .unwrap(),
        CancelOutcome::Requested
    );
    stub.release.notify_one();
    let report = task.await.unwrap();
    assert_eq!(report.state, RunState::Cancelled);
    assert_eq!(report.counts.seen, 200);
    assert_eq!(node.memory_rows(&alice).await, 0);
    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_budget_and_the_rate_are_the_runners_to_enforce() {
    // B-8.
    let node = node().await;
    let alice = scope("alice");
    let stub = Stub::new("notes", SourceKind::Authenticated, fixture(1_000));
    let runner = runner(std::slice::from_ref(&stub), IngestPolicy::default());
    let runtime = node.runtime();

    let mut budgeted = request(&alice, "notes");
    budgeted.limits = RunLimits {
        max_items: 250,
        ..quick()
    };
    let report = runner.run(&runtime, budgeted).await.unwrap();
    assert_eq!(report.state, RunState::Exhausted, "{report:?}");
    assert_eq!(report.counts.seen, 250);
    assert_eq!(
        stub.limits(),
        [100, 100, 50],
        "the adapter is asked for what is left of the budget and no more"
    );
    assert_eq!(node.cursor(&alice, "notes").await.as_deref(), Some("250"));

    // The next run takes up the rest.
    let rest = runner
        .run(&runtime, request(&alice, "notes"))
        .await
        .unwrap();
    assert_eq!(rest.state, RunState::Completed);
    assert_eq!(rest.counts.seen, 750);

    let slow = Stub::new("slow", SourceKind::Authenticated, fixture(300));
    let runner = common::runner(&[slow], IngestPolicy::default());
    let mut paced = request(&alice, "slow");
    paced.limits = RunLimits {
        rate_per_second: 200,
        ..quick()
    };
    let began = Instant::now();
    let report = runner.run(&runtime, paced).await.unwrap();
    assert_eq!(report.state, RunState::Completed);
    assert!(
        began.elapsed() >= Duration::from_millis(900),
        "300 items at 200 a second take over a second, not {:?}",
        began.elapsed()
    );
    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_real_run_per_source_and_scope_at_a_time() {
    let node = node().await;
    let alice = scope("alice");
    let stub = Stub::new("notes", SourceKind::Authenticated, fixture(10));
    let runner = runner(&[stub], IngestPolicy::default());
    let runtime = node.runtime();

    let first = runner
        .start(&runtime, request(&alice, "notes"))
        .await
        .unwrap();
    let second = runner.start(&runtime, request(&alice, "notes")).await;
    assert!(matches!(second, Err(Error::Conflict(_))), "{second:?}");
    // Another scope is another run; a dry run writes no row to conflict with.
    runner
        .start(&runtime, request(&scope("bob"), "notes"))
        .await
        .expect("another scope");
    let mut dry = request(&alice, "notes");
    dry.dry_run = true;
    runner.start(&runtime, dry).await.expect("a dry run");

    let report = runner.drive(&runtime, first).await;
    assert_eq!(report.state, RunState::Completed);
    runner
        .start(&runtime, request(&alice, "notes"))
        .await
        .expect("a finished run no longer blocks");

    let unknown = runner.start(&runtime, request(&alice, "nowhere")).await;
    assert!(matches!(unknown, Err(Error::NotFound(_))), "{unknown:?}");
    let mut greedy = request(&alice, "notes");
    greedy.limits.max_items = 0;
    assert!(matches!(
        runner.start(&runtime, greedy).await,
        Err(Error::Validation(_))
    ));
    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_page_commits_only_if_its_run_and_its_cursor_are_still_the_ones_it_began_with() {
    // The guard of every transaction: a run that was taken over, and a cursor
    // that moved or appeared under it, abort the page whole.
    let node = node().await;
    let alice = scope("alice");
    let scope_id = aicortex_store::ScopeId::of(&alice);

    // The run's row stops saying running while a page is in hand.
    let stub = Stub::new("notes", SourceKind::Authenticated, fixture(300));
    stub.set_faults(Faults {
        park_at: Some(1),
        ..Faults::default()
    });
    let runner = runner(std::slice::from_ref(&stub), IngestPolicy::default());
    let runtime = node.runtime();
    let started = runner
        .start(&runtime, request(&alice, "notes"))
        .await
        .unwrap();
    let task = tokio::spawn({
        let (runner, runtime) = (runner.clone(), runtime.clone());
        async move { runner.drive(&runtime, started).await }
    });
    stub.reached.notified().await;
    node.exec(
        "UPDATE ingest_run SET state = 'failed', error_code = 'abandoned',
             finished = 1, touched = 1 WHERE scope_id = $1",
        vec![Value::from(&scope_id)],
    )
    .await;
    stub.release.notify_one();
    let report = task.await.unwrap();
    assert_eq!(report.state, RunState::Failed);
    assert_eq!(
        node.memory_rows(&alice).await,
        100,
        "the second page stored nothing"
    );
    assert_eq!(node.cursor(&alice, "notes").await.as_deref(), Some("100"));

    // The stored cursor changes under a page that expects the one it fetched
    // from.
    let moved = Stub::new("moved", SourceKind::Authenticated, fixture(300));
    moved.set_faults(Faults {
        park_at: Some(1),
        ..Faults::default()
    });
    let runner = common::runner(std::slice::from_ref(&moved), IngestPolicy::default());
    let started = runner
        .start(&runtime, request(&alice, "moved"))
        .await
        .unwrap();
    let task = tokio::spawn({
        let (runner, runtime) = (runner.clone(), runtime.clone());
        async move { runner.drive(&runtime, started).await }
    });
    moved.reached.notified().await;
    node.exec(
        "UPDATE ingest_cursor SET cursor = '999' WHERE scope_id = $1 AND source_id = 'moved'",
        vec![Value::from(&scope_id)],
    )
    .await;
    moved.release.notify_one();
    let report = task.await.unwrap();
    assert_eq!(report.failure, Some(RunFailure::StoreFailed), "{report:?}");
    assert_eq!(node.cursor(&alice, "moved").await.as_deref(), Some("999"));

    // A cursor appears where the page expects none.
    let fresh = Stub::new("fresh", SourceKind::Authenticated, fixture(300));
    fresh.set_faults(Faults {
        park_at: Some(0),
        ..Faults::default()
    });
    let runner = common::runner(std::slice::from_ref(&fresh), IngestPolicy::default());
    let started = runner
        .start(&runtime, request(&alice, "fresh"))
        .await
        .unwrap();
    let task = tokio::spawn({
        let (runner, runtime) = (runner.clone(), runtime.clone());
        async move { runner.drive(&runtime, started).await }
    });
    fresh.reached.notified().await;
    node.exec(
        "INSERT INTO ingest_cursor (scope_id, source_id, cursor, run_id, touched)
         VALUES ($1, 'fresh', '7', 'run-elsewhere', 1)",
        vec![Value::from(&scope_id)],
    )
    .await;
    fresh.release.notify_one();
    let report = task.await.unwrap();
    assert_eq!(report.failure, Some(RunFailure::StoreFailed), "{report:?}");
    assert_eq!(
        node.count(
            "SELECT COUNT(*) AS count FROM ingest_item WHERE source_id = 'fresh'",
            vec![]
        )
        .await,
        0,
        "nothing of the aborted page is stored"
    );
    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_run_whose_row_is_gone_stops_as_superseded_and_stores_nothing_more() {
    // The row can vanish from under a run when its scope is erased.
    let node = node().await;
    let alice = scope("alice");
    let stub = Stub::new("notes", SourceKind::Authenticated, fixture(300));
    stub.set_faults(Faults {
        park_at: Some(1),
        ..Faults::default()
    });
    let runner = runner(std::slice::from_ref(&stub), IngestPolicy::default());
    let runtime = node.runtime();
    let started = runner
        .start(&runtime, request(&alice, "notes"))
        .await
        .unwrap();
    let task = tokio::spawn({
        let (runner, runtime) = (runner.clone(), runtime.clone());
        async move { runner.drive(&runtime, started).await }
    });
    stub.reached.notified().await;
    node.exec("DELETE FROM ingest_run WHERE source_id = 'notes'", vec![])
        .await;
    stub.release.notify_one();

    let report = task.await.unwrap();
    assert_eq!(report.state, RunState::Failed, "{report:?}");
    assert_eq!(report.failure, Some(RunFailure::Superseded));
    assert_eq!(node.memory_rows(&alice).await, 100);
    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cancel_is_seen_between_retries_and_not_after_them() {
    // The wait before a retry is not a place a cancel can hide.
    let node = node().await;
    let alice = scope("alice");
    let stub = Stub::new("notes", SourceKind::Authenticated, fixture(500));
    stub.set_faults(Faults {
        transient_at: Some(1),
        ..Faults::default()
    });
    let runner = runner(std::slice::from_ref(&stub), IngestPolicy::default());
    let runtime = node.runtime();
    let mut slow = request(&alice, "notes");
    slow.limits = RunLimits {
        retries: 10,
        backoff: Duration::from_secs(2),
        ..quick()
    };
    let started = runner.start(&runtime, slow).await.unwrap();
    let run = RunId::parse(started.run_id().as_str()).unwrap();
    let task = tokio::spawn({
        let (runner, runtime) = (runner.clone(), runtime.clone());
        async move { runner.drive(&runtime, started).await }
    });
    while stub.served.load(std::sync::atomic::Ordering::SeqCst) < 3 {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let began = Instant::now();
    assert_eq!(
        runner
            .cancel(&node.handle(), &alice, &source("notes"), &run)
            .await
            .unwrap(),
        CancelOutcome::Requested
    );
    let report = task.await.unwrap();
    assert_eq!(report.state, RunState::Cancelled, "{report:?}");
    assert!(
        began.elapsed() < Duration::from_secs(2),
        "the run stopped in {:?}, well inside one backoff",
        began.elapsed()
    );
    assert_eq!(report.counts.seen, 100);
    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dry_runs_are_bounded_in_what_they_look_at_and_in_how_many_run_at_once() {
    let node = node().await;
    let alice = scope("alice");
    let runner = runner(
        &[Stub::new("notes", SourceKind::Authenticated, fixture(10))],
        IngestPolicy::default(),
    );
    let runtime = node.runtime();

    let mut greedy = request(&alice, "notes");
    greedy.dry_run = true;
    greedy.limits.max_items = aicortex_ingest::MAX_DRY_RUN_ITEMS + 1;
    assert!(matches!(
        runner.start(&runtime, greedy).await,
        Err(Error::Validation(_))
    ));

    let mut dry = request(&alice, "notes");
    dry.dry_run = true;
    let mut started = Vec::new();
    for _ in 0..aicortex_ingest::MAX_BOARD_RUNS {
        started.push(runner.start(&runtime, dry.clone()).await.unwrap());
    }
    assert!(matches!(
        runner.start(&runtime, dry.clone()).await,
        Err(Error::Conflict(_))
    ));
    // Once one has ended, the board makes room by forgetting it.
    let finished = started.pop().unwrap();
    let forgotten = RunId::parse(finished.run_id().as_str()).unwrap();
    runner.drive(&runtime, finished).await;
    runner.start(&runtime, dry).await.expect("room was made");
    assert!(
        runner
            .report(&node.handle(), &alice, &source("notes"), &forgotten)
            .await
            .unwrap()
            .is_none(),
        "the finished preview was the one forgotten"
    );
    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_changed_item_supersedes_what_it_replaces_and_an_unchanged_one_is_updated() {
    // B-4.
    let node = node().await;
    let alice = scope("alice");
    let runtime = node.runtime();

    let v1 = Stub::new("notes", SourceKind::Authenticated, fixture(20));
    let first = runner(&[v1], IngestPolicy::default());
    first.run(&runtime, request(&alice, "notes")).await.unwrap();

    let mut edited = fixture(20);
    for (index, item) in edited.iter_mut().enumerate().take(5) {
        item.payload = serde_json::json!({ "text": format!("edited {index}") });
    }
    let v2 = Stub::new("notes", SourceKind::Authenticated, edited);
    let second = runner(&[v2], IngestPolicy::default());
    let report = second
        .run(&runtime, request(&alice, "notes"))
        .await
        .unwrap();
    assert_eq!(
        report.counts,
        RunCounts {
            seen: 20,
            updated: 20,
            ..RunCounts::default()
        }
    );
    assert_eq!(node.memory_rows(&alice).await, 25);
    assert_eq!(node.item_rows(&alice).await, 20);
    assert_eq!(node.bodies(&alice, "superseded").await, texts(0..5));
    let mut active = texts(5..20);
    active.extend((0..5).map(|index| format!("edited {index}")));
    active.sort();
    assert_eq!(node.bodies(&alice, "active").await, active);

    // The mapping points at the live memory.
    let row = ItemRepo::lookup(&node.handle(), &alice, &source("notes"), "ext-0002")
        .await
        .unwrap()
        .unwrap();
    let live = node
        .memories(&alice)
        .await
        .into_iter()
        .find(|memory| memory.id == row.memory)
        .unwrap();
    assert_eq!(live.body.text, "edited 2");
    assert_eq!(live.status.label(), "active");
    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn erasing_a_memory_takes_its_mapping_and_a_reimport_does_not_revive_it() {
    // Forgetting is real: the mapping and its digest are derivatives.
    let node = node().await;
    let alice = scope("alice");
    let runtime = node.runtime();
    let runner = runner(
        &[Stub::new("notes", SourceKind::Authenticated, fixture(5))],
        IngestPolicy::default(),
    );
    runner
        .run(&runtime, request(&alice, "notes"))
        .await
        .unwrap();

    let row = ItemRepo::lookup(&node.handle(), &alice, &source("notes"), "ext-0002")
        .await
        .unwrap()
        .unwrap();
    let authority = Authority::of(Sub::new("alice"));
    Eraser::new()
        .erase(
            &node.handle(),
            &node.ledger,
            &Erasure::new(
                &alice,
                row.memory,
                &authority,
                UnixSeconds::new(1_800_000_000),
            ),
        )
        .await
        .expect("the erasure runs");
    assert_eq!(node.item_rows(&alice).await, 4, "the mapping went with it");
    assert!(
        ItemRepo::lookup(&node.handle(), &alice, &source("notes"), "ext-0002")
            .await
            .unwrap()
            .is_none()
    );

    let again = runner
        .run(&runtime, request(&alice, "notes"))
        .await
        .unwrap();
    assert_eq!(
        again.counts,
        RunCounts {
            seen: 5,
            admitted: 1,
            updated: 4,
            ..RunCounts::default()
        },
        "the erased item comes back as a new capture"
    );
    let fresh = ItemRepo::lookup(&node.handle(), &alice, &source("notes"), "ext-0002")
        .await
        .unwrap()
        .unwrap();
    assert_ne!(fresh.memory, row.memory, "the tombstone is not revived");
    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn progress_is_published_as_ids_and_states_and_never_content() {
    // B-2, 020 B-6.
    let node = node().await;
    let alice = scope("alice");
    let runtime = node.runtime();
    let mut events = runtime.events.subscribe();
    let stub = Stub::new("notes", SourceKind::Authenticated, fixture(250));
    stub.set_faults(Faults {
        fatal_at: Some(2),
        ..Faults::default()
    });
    let runner = runner(&[stub], IngestPolicy::default());

    let report = runner
        .run(&runtime, request(&alice, "notes"))
        .await
        .unwrap();
    let mut seen = Vec::new();
    while let Ok(event) = events.try_recv() {
        seen.push(event);
    }
    assert!(seen.iter().all(|event| event.name == "ingest"));
    assert!(
        seen.iter()
            .all(|event| event.data.id.as_deref() == Some(report.run_id.as_str()))
    );
    assert!(seen.iter().all(|event| event.owner == alice.owner));
    let states: Vec<_> = seen.iter().map(|event| event.data.state.as_str()).collect();
    assert_eq!(states, ["running", "running", "failed"]);
    assert_eq!(
        seen.last().unwrap().data.reason_code.as_deref(),
        Some("adapter_failed")
    );
    let wire =
        serde_json::to_string(&seen.iter().map(|event| &event.data).collect::<Vec<_>>()).unwrap();
    assert!(!wire.contains("fixture note"), "no content on the bus");
    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_decision_that_cannot_be_appended_stops_the_run_after_the_page_that_committed() {
    // Items are stored in one transaction and their Decisions appended after
    // it (013 B-9). A chain that cannot take them stops the run with a code,
    // at the cursor the commit left, so the page is not replayed.
    let node = node().await;
    let broken = node.broken_ledger().await;
    let alice = scope("alice");
    let secret = concat!("gh", "p_", "9wQk2LmXr4Tb8Zc1Nd6Vf3Hs5Jg7Pa0Ye2Uu");
    let mut items = fixture(150);
    items[10] = RawItem::new(
        "ext-leak",
        UnixSeconds::new(1_700_001_000),
        serde_json::json!({ "text": format!("the key is {secret}") }),
    );
    let runner = runner(
        &[Stub::new("notes", SourceKind::Authenticated, items)],
        IngestPolicy::default(),
    );

    let report = runner
        .run(&node.runtime_with(broken), request(&alice, "notes"))
        .await
        .unwrap();
    assert_eq!(report.state, RunState::Failed, "{report:?}");
    assert_eq!(report.failure, Some(RunFailure::LedgerUnavailable));
    assert_eq!(report.counts.refused, 1);
    assert_eq!(node.memory_rows(&alice).await, 99, "the page committed");
    assert_eq!(
        node.cursor(&alice, "notes").await.as_deref(),
        Some("100"),
        "and the cursor moved with it"
    );
    node.shutdown().await;
}
