//! One chain, referenced (spec 024 AC-1).
//!
//! Every assertion is made against a real hiqlite node and the chassis's own
//! chain: the Decisions are appended by the shipped erasure, the references
//! are staged through the shipped repository, and `Ledger::verify` is rahi's.
//! Nothing here chains, hashes or signs anything itself, which is B-1.
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

use std::path::{Path, PathBuf};

use aicortex_store::{
    Authority, DecisionRefRepo, Eraser, Erasure, EventKind, History, KIND_ERASE, Lifecycle,
    MAX_JUSTIFICATION_BYTES, MAX_REASON_BYTES, ReasonCode, RefKind, append_decision, history,
    validate_decision,
};
use aicortex_types::{Memory, MemoryId, MemoryKind, Scope};
use rahi_ledger::{Decision, DecisionId, DecisionKind, Outcome};
use rahi_store::{TxnBuilder, Value};
use rahi_types::{Error, Sub, UnixSeconds};
use serde_json::json;

const T0: u64 = 1_700_000_000;

async fn capture(node: &common::Node, memory: &Memory) -> MemoryId {
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
    captured.id()
}

async fn correct(node: &common::Node, correction: &Memory, old: MemoryId) {
    let admitted = common::admit(correction);
    let work = common::work(correction);
    let mut txn = TxnBuilder::new();
    Lifecycle::new()
        .correct(&node.handle(), &mut txn, &admitted, old, &work)
        .await
        .expect("the correction stages");
    node.handle()
        .txn(txn.into_statements())
        .await
        .expect("the correction commits");
}

async fn refer(
    node: &common::Node,
    scope: &Scope,
    memory: MemoryId,
    decision: &str,
    kind: RefKind,
    at: u64,
) {
    let mut txn = TxnBuilder::new();
    DecisionRefRepo::stage(
        &mut txn,
        scope,
        memory,
        decision,
        kind,
        UnixSeconds::new(at),
    );
    node.handle()
        .txn(txn.into_statements())
        .await
        .expect("the reference commits");
}

fn kinds(history: &History) -> Vec<&'static str> {
    history
        .events
        .iter()
        .map(|event| event.event.label())
        .collect()
}

fn decision(kind: &str, id: &str, payload: serde_json::Value) -> Decision {
    Decision::new(
        DecisionId::new(id),
        DecisionKind::new(kind),
        Sub::new("sub-alice"),
        Outcome::Allow,
        "a test decision",
    )
    .with_payload(payload)
}

fn valid_payload() -> serde_json::Value {
    json!({ "scope": "personal:sub-alice", "reason_code": "promotion", "memory_id": "m" })
}

// ---------------------------------------------------------------- B-4

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b4_the_timeline_merges_lifecycle_rows_and_decisions_in_time_order() {
    let node = common::node().await;
    let alice = common::scope("alice");
    let text = "the standup moved to nine";

    let original = common::memory(&alice, text, T0);
    let original_id = capture(&node, &original).await;
    // The same content again: a merge, not a second row.
    let repeat = common::memory(&alice, text, T0 + 100);
    assert_eq!(capture(&node, &repeat).await, original_id);

    refer(
        &node,
        &alice,
        original_id,
        "memory.promote-1",
        RefKind::Promote,
        T0 + 200,
    )
    .await;

    // A correction of the original: the original is corrected, and the
    // correction names what it corrects.
    let correction = common::derived_memory(
        &alice,
        "the standup moved to ten",
        T0 + 300,
        MemoryKind::Correction,
        vec![original_id],
    );
    correct(&node, &correction, original_id).await;

    let old = history(&node.handle(), &alice, original_id).await.unwrap();
    assert_eq!(kinds(&old), ["created", "merged", "promote", "corrected"]);
    assert_eq!(old.events[2].decision.as_deref(), Some("memory.promote-1"));
    assert_eq!(old.events[3].related, Some(correction.id));
    assert!(
        old.events.windows(2).all(|pair| pair[0].at <= pair[1].at),
        "the timeline is ordered by time: {old:?}"
    );
    assert!(!old.truncated);

    let new = history(&node.handle(), &alice, correction.id)
        .await
        .unwrap();
    assert_eq!(kinds(&new), ["created", "corrects"]);
    assert_eq!(new.events[1].related, Some(original_id));

    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b4_a_history_never_reaches_another_scope() {
    let node = common::node().await;
    let alice = common::scope("alice");
    let bob = common::scope("bob");
    let id = capture(&node, &common::memory(&alice, "alice's note", T0)).await;
    refer(
        &node,
        &alice,
        id,
        "memory.promote-2",
        RefKind::Promote,
        T0 + 1,
    )
    .await;

    assert!(history(&node.handle(), &alice, id).await.is_ok());
    let error = history(&node.handle(), &bob, id).await.unwrap_err();
    assert!(
        matches!(error, Error::NotFound(_)),
        "another scope's memory is not found, never an empty history: {error:?}"
    );
    let missing = history(&node.handle(), &alice, MemoryId::now_v7())
        .await
        .unwrap_err();
    assert!(matches!(missing, Error::NotFound(_)));

    node.shutdown().await;
}

// ---------------------------------------------------------------- FR-004

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr004_ten_events_cost_one_statement_whatever_the_chain_holds() {
    let node = common::node().await;
    let alice = common::scope("alice");
    let first = common::memory(&alice, "ten events", T0);
    let id = capture(&node, &first).await;
    for step in 1..=4 {
        let repeat = common::memory(&alice, "ten events", T0 + step);
        assert_eq!(capture(&node, &repeat).await, id);
    }
    for (index, kind) in [
        RefKind::Promote,
        RefKind::Demote,
        RefKind::Promote,
        RefKind::Demote,
        RefKind::Lapse,
    ]
    .into_iter()
    .enumerate()
    {
        refer(
            &node,
            &alice,
            id,
            &format!("memory.event-{index}"),
            kind,
            T0 + 10 + u64::try_from(index).unwrap(),
        )
        .await;
    }

    let short = history(&node.handle(), &alice, id).await.unwrap();
    assert_eq!(short.events.len(), 10, "{short:?}");

    // A chain a hundred records longer, none of them about this memory.
    for index in 0..100 {
        node.ledger
            .append(decision(
                "memory.filler",
                &format!("filler-{index}"),
                valid_payload(),
            ))
            .await
            .unwrap();
    }
    let long = history(&node.handle(), &alice, id).await.unwrap();
    assert_eq!(long, short, "the chain's length changes nothing");
    assert_eq!(long.statements, 1, "one bounded statement, not a scan");

    node.shutdown().await;
}

// ---------------------------------------------------------------- B-5

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b5_an_erased_memory_keeps_its_history_and_the_chain_still_verifies() {
    let node = common::node().await;
    let alice = common::scope("alice");
    let secret_words = "zanzibar marmalade umbrella";
    let memory = common::memory(&alice, secret_words, T0);
    let id = capture(&node, &memory).await;
    refer(
        &node,
        &alice,
        id,
        "memory.promote-3",
        RefKind::Promote,
        T0 + 5,
    )
    .await;

    let authority = Authority::of(common::sub("alice")).because("subject_request");
    let erased = Eraser::new()
        .erase(
            &node.handle(),
            &node.ledger,
            &Erasure::new(&alice, id, &authority, UnixSeconds::new(T0 + 50)),
        )
        .await
        .unwrap();

    let after = history(&node.handle(), &alice, id).await.unwrap();
    assert_eq!(kinds(&after), ["created", "promote", "erase", "erased"]);
    assert_eq!(
        after.events[2].decision.as_deref(),
        Some(erased.decision.as_str()),
        "the erase event names the Decision on the chain"
    );
    assert!(matches!(
        after.events[2].event,
        EventKind::Decision(RefKind::Erase)
    ));

    // FR-003: no substring of the erased body is in what the chain holds, and
    // the Decision is one of ours, so it validates.
    let mut seen = 0;
    for record in node.ledger.records().await.unwrap() {
        let decision = record.decision().unwrap();
        let serialized = serde_json::to_string(&decision).unwrap();
        for word in secret_words.split(' ') {
            assert!(!serialized.contains(word), "{word} is in {serialized}");
        }
        if decision.kind.as_str() == KIND_ERASE {
            assert_eq!(
                validate_decision(&decision).unwrap(),
                ReasonCode::SubjectRequest
            );
            seen += 1;
        }
    }
    assert_eq!(seen, 1);

    // FR-005's store half: erasure leaves the chain verifiable.
    node.ledger.verify().await.unwrap();

    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b2_an_erasure_under_a_code_outside_the_closed_set_is_refused_before_it_stages() {
    let node = common::node().await;
    let alice = common::scope("alice");
    let id = capture(&node, &common::memory(&alice, "keep me", T0)).await;

    let authority = Authority::of(common::sub("alice")).because("because I felt like it");
    let error = Eraser::new()
        .erase(
            &node.handle(),
            &node.ledger,
            &Erasure::new(&alice, id, &authority, UnixSeconds::new(T0 + 1)),
        )
        .await
        .unwrap_err();
    assert!(matches!(error, Error::Validation(_)), "{error:?}");
    let after = history(&node.handle(), &alice, id).await.unwrap();
    assert_eq!(kinds(&after), ["created"], "nothing was staged");

    node.shutdown().await;
}

// ---------------------------------------------------------------- B-2, B-3

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b3_a_reference_is_idempotent_and_every_kind_is_accepted_by_the_table() {
    let node = common::node().await;
    let alice = common::scope("alice");
    let id = capture(&node, &common::memory(&alice, "references", T0)).await;

    for (index, kind) in RefKind::ALL.into_iter().enumerate() {
        refer(
            &node,
            &alice,
            id,
            &format!("memory.k-{index}"),
            kind,
            T0 + 1,
        )
        .await;
    }
    // A replay of the same Decision is the same record.
    refer(
        &node,
        &alice,
        id,
        "memory.k-0",
        RefKind::Quarantine,
        T0 + 99,
    )
    .await;
    let rows = common::count(
        &node,
        "SELECT COUNT(*) AS count FROM decision_ref WHERE scope_id = $1 AND memory_id = $2",
        vec![
            Value::from(&aicortex_store::ScopeId::of(&alice)),
            Value::from(id.to_string()),
        ],
    )
    .await;
    assert_eq!(rows, 8);

    let refused = node
        .handle()
        .txn(vec![rahi_store::Statement::with_params(
            "INSERT INTO decision_ref (scope_id, memory_id, decision_id, kind, created)
             VALUES ('s', 'm', 'd', 'invented', 1)",
            vec![],
        )])
        .await;
    assert!(refused.is_err(), "the kind column is a closed set");

    node.shutdown().await;
}

#[test]
fn b2_the_reason_codes_are_a_closed_round_tripping_set() {
    for code in ReasonCode::ALL {
        assert_eq!(ReasonCode::parse(code.label()), Some(code));
    }
    for kind in RefKind::ALL {
        assert_eq!(RefKind::parse(kind.label()), Some(kind));
    }
    assert_eq!(ReasonCode::parse("anything else"), None);
    let mut labels: Vec<_> = ReasonCode::ALL.iter().map(|code| code.label()).collect();
    labels.sort_unstable();
    labels.dedup();
    assert_eq!(labels.len(), ReasonCode::ALL.len(), "labels are unique");

    // The gate's codes are a subset of this set: one vocabulary, two crates.
    let gate = aicortex_gate::Gate::standard();
    let secret = common::memory(
        &common::scope("alice"),
        "token ghp_abcdefghijklmnopqrstuvwxyz0123456789",
        T0,
    );
    let verdict = gate.evaluate(&aicortex_gate::Candidate::new(
        common::parts_of(&secret),
        aicortex_gate::Origin::Authenticated,
    ));
    let reason = verdict.reason().expect("a secret is refused");
    assert!(
        ReasonCode::parse(reason.code()).is_some(),
        "{}",
        reason.code()
    );
}

#[test]
fn b2_a_decision_that_could_carry_content_does_not_validate() {
    assert_eq!(
        validate_decision(&decision("memory.x", "ok", valid_payload())).unwrap(),
        ReasonCode::Promotion
    );

    let invalid = [
        ("a null payload", serde_json::Value::Null),
        ("no code", json!({ "scope": "s" })),
        (
            "an unknown code",
            json!({ "scope": "s", "reason_code": "vibes" }),
        ),
        ("no scope", json!({ "reason_code": "promotion" })),
        (
            "an empty scope",
            json!({ "scope": "", "reason_code": "promotion" }),
        ),
        (
            "a body",
            json!({ "scope": "s", "reason_code": "promotion", "body": "hello" }),
        ),
        (
            "a nested content key",
            json!({ "scope": "s", "reason_code": "promotion", "detail": [{ "Content": "x" }] }),
        ),
        (
            "an embedding",
            json!({ "scope": "s", "reason_code": "promotion", "embedding": [0.1] }),
        ),
        (
            "a long justification",
            json!({
                "scope": "s", "reason_code": "promotion",
                "justification": "x".repeat(MAX_JUSTIFICATION_BYTES + 1),
            }),
        ),
        (
            "a justification that is not text",
            json!({ "scope": "s", "reason_code": "promotion", "justification": 7 }),
        ),
        (
            "a long nested reviewer reason",
            json!({
                "scope": "s", "reason_code": "promotion",
                "resolution": { "reason": "x".repeat(MAX_REASON_BYTES + 1) },
            }),
        ),
    ];
    for (name, payload) in invalid {
        let error = validate_decision(&decision("memory.x", "bad", payload)).unwrap_err();
        assert!(matches!(error, Error::Validation(_)), "{name}: {error:?}");
        assert!(
            !error.to_string().contains("hello"),
            "{name}: the message repeats a value"
        );
    }

    let short = json!({
        "scope": "s", "reason_code": "promotion",
        "justification": "x".repeat(MAX_JUSTIFICATION_BYTES),
    });
    assert!(validate_decision(&decision("memory.x", "ok2", short)).is_ok());
    let reason = json!({
        "scope": "s", "reason_code": "promotion",
        "resolution": { "reason": "x".repeat(MAX_REASON_BYTES) },
    });
    assert!(validate_decision(&decision("memory.x", "ok3", reason)).is_ok());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b1_the_validated_append_refuses_before_the_chain_sees_it() {
    let node = common::node().await;
    let before = node.ledger.count().await.unwrap();
    let bad = decision("memory.x", "never-appended", json!({ "scope": "s" }));
    assert!(append_decision(&node.ledger, bad).await.is_err());
    assert_eq!(node.ledger.count().await.unwrap(), before);

    append_decision(
        &node.ledger,
        decision("memory.x", "appended", valid_payload()),
    )
    .await
    .unwrap();
    assert_eq!(node.ledger.count().await.unwrap(), before + 1);
    node.ledger.verify().await.unwrap();

    node.shutdown().await;
}

// ---------------------------------------------------------------- FR-001

/// Where a source file's unit test module begins (an attribute on a `mod`,
/// not on an import).
const CFG_TEST: &str = "#[cfg(test)]\nmod ";

/// Names that would mean this workspace keeps, verifies or signs a chain of
/// its own: a hash-link column, a verification routine, a signer. Reading a
/// field of rahi's own `Decision` is not one of them, so the bare field name
/// is only forbidden where it is a column definition. The pieces are joined
/// at run time so this file does not contain what it searches for.
fn forbidden() -> Vec<String> {
    [
        ["parent", "_hash"],
        ["record", "_hash"],
        ["previous", "_record_hash"],
        ["prev", "_hash TEXT"],
        ["prev", "_hash BLOB"],
        ["fn ", "verify_chain"],
        ["Ledger", "Signer"],
    ]
    .iter()
    .map(|parts| parts.concat())
    .collect()
}

fn sources(root: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            sources(&path, found);
        } else if path
            .extension()
            .is_some_and(|ext| ext == "rs" || ext == "sql")
        {
            found.push(path);
        }
    }
}

#[test]
fn fr001_no_chain_of_our_own_exists_in_the_workspace() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("the workspace root")
        .to_path_buf();
    let mut files = Vec::new();
    for tree in ["crates", "apps"] {
        let mut crates = Vec::new();
        if let Ok(entries) = std::fs::read_dir(workspace.join(tree)) {
            for entry in entries.flatten() {
                crates.push(entry.path().join("src"));
            }
        }
        for src in crates {
            sources(&src, &mut files);
        }
    }
    assert!(
        files.len() > 50,
        "the scan found only {} files",
        files.len()
    );
    let names = forbidden();
    let mut hits = Vec::new();
    for file in &files {
        // Production code only: a unit test module may read rahi's chain back
        // to check what this crate appended to it, and says so at the top of
        // the module with its own cfg attribute.
        let full = std::fs::read_to_string(file).unwrap();
        let text = full.split(CFG_TEST).next().unwrap_or_default();
        for name in &names {
            if text.contains(name.as_str()) {
                hits.push(format!("{} names {name}", file.display()));
            }
        }
    }
    assert!(hits.is_empty(), "{hits:#?}");
}
