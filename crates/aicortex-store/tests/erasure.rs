//! Forgetting, proved rather than described (spec 014 AC-1, AC-2).
//!
//! Every assertion here is made by reading the store and the chain back after
//! the shipped code has run against a real hiqlite node and a real rahi
//! ledger. Nothing is stubbed: the gate that quarantines a capture is the
//! shipped gate, the digest key is minted by the shipped `DecisionKey`, and
//! the chain is the chassis's own.
//!
//! The care taken over FR-007 is deliberate. "The key is gone" is the kind of
//! claim that passes vacuously if the key was never there, so every test that
//! asserts an absence first establishes the presence: the key is read back,
//! and the digest the Decision carries is *recomputed with it* and checked,
//! before anything is erased. Only then does the absence afterwards mean
//! something.
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

use common::backup_bytes;

use aicortex_gate::{Candidate, DigestRef, Gate, KIND_QUARANTINE, Origin, Verdict};
use aicortex_store::{
    Authority, Counters, DERIVATIVES, Derivative, Erased, Eraser, Erasure, KIND_ERASE,
    KIND_ERASE_SCOPE, Lifecycle, MAX_ERASURE_BATCH, MemoryFilter, MemoryRepo, PLANNED, ScopeId,
    StatusFilter, erasure_lease_key, fingerprint,
};
use aicortex_types::{AicortexTime, Memory, MemoryId, MemoryKind, Scope, Status};
use rahi_ledger::SignedRecord;
use rahi_store::{LEASE_TTL_SECONDS, Outbox, Statement, TxnBuilder, Value};
use rahi_types::UnixSeconds;

/// The two tables spec 015 will own, created here so that FR-003 can be
/// asserted over real rows rather than over the absence of a table.
///
/// The column names are [`PLANNED`]'s, so the day 015 lands and moves its
/// entries into [`DERIVATIVES`] the sweep is already the one being exercised.
const FUTURE_TABLES: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS chunk (
        chunk_id TEXT PRIMARY KEY, memory_id TEXT NOT NULL, scope_id TEXT NOT NULL,
        ordinal INTEGER NOT NULL)",
    "CREATE TABLE IF NOT EXISTS embedding (
        embedding_id TEXT PRIMARY KEY, memory_id TEXT NOT NULL, scope_id TEXT NOT NULL,
        model TEXT NOT NULL, vector BLOB NOT NULL)",
    "CREATE TABLE IF NOT EXISTS chunk_token (
        chunk_id TEXT NOT NULL, memory_id TEXT NOT NULL, scope_id TEXT NOT NULL,
        token TEXT NOT NULL, tf INTEGER NOT NULL, PRIMARY KEY (chunk_id, token))",
];

/// An eraser that also sweeps the tables specs 015 and 016 will bring.
fn eraser_with_future_tables() -> Eraser {
    PLANNED
        .iter()
        .fold(Eraser::new(), |eraser, planned| eraser.also(*planned))
}

/// Create the future tables and put one row per table behind `memory`.
async fn seed_derivatives(node: &common::Node, scope: &Scope, memory: MemoryId) {
    let scope_id = ScopeId::of(scope);
    for ddl in FUTURE_TABLES {
        node.handle()
            .execute(*ddl, vec![])
            .await
            .expect("the future table is created");
    }
    node.handle()
        .txn(vec![
            Statement::with_params(
                "INSERT INTO chunk (chunk_id, memory_id, scope_id, ordinal)
                 VALUES ($1, $2, $3, 0)",
                vec![
                    Value::from(format!("chunk-{memory}")),
                    Value::from(memory.to_string()),
                    Value::from(&scope_id),
                ],
            ),
            Statement::with_params(
                "INSERT INTO embedding (embedding_id, memory_id, scope_id, model, vector)
                 VALUES ($1, $2, $3, 'test-model', $4)",
                vec![
                    Value::from(format!("embedding-{memory}")),
                    Value::from(memory.to_string()),
                    Value::from(&scope_id),
                    Value::Blob(vec![1, 2, 3, 4]),
                ],
            ),
            Statement::with_params(
                "INSERT INTO chunk_token (chunk_id, memory_id, scope_id, token, tf)
                 VALUES ($1, $2, $3, 'token', 1)",
                vec![
                    Value::from(format!("chunk-{memory}")),
                    Value::from(memory.to_string()),
                    Value::from(&scope_id),
                ],
            ),
        ])
        .await
        .expect("the derivative rows commit");
}

/// Capture through the merge-aware path and commit (014 B-2).
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

/// What a quarantined capture left behind: the row, its key, and the digest
/// the Decision carries.
#[allow(dead_code)]
struct Quarantined {
    memory: MemoryId,
    key_id: aicortex_store::DecisionKeyId,
    digest: DigestRef,
    material: Vec<u8>,
}

/// Offer a candidate the gate will quarantine and do what 013 B-9 requires.
///
/// This is the composition spec 013's own tests exercise, repeated here
/// because FR-007 is a claim about what erasure does to *that* composition's
/// output: the row, the key row that covers it, and the Decision that names
/// the key. Anything less than the real thing would be asserting about a
/// fixture rather than about the system.
async fn quarantine(node: &common::Node, memory: &Memory) -> Quarantined {
    let gate = Gate::standard();
    let candidate = Candidate::new(common::parts_of(memory), Origin::Asserted);
    let verdict = gate.evaluate(&candidate);
    let Verdict::Quarantine(admitted, reason) = verdict else {
        panic!("an asserted origin must quarantine, not {verdict:?}");
    };

    let key = aicortex_store::DecisionKey::mint().expect("a key mints");
    let material = gate.digest_material(&candidate);
    let digest = DigestRef {
        digest: key.digest(&material),
        algorithm: aicortex_store::DIGEST_ALGORITHM.to_owned(),
        key_id: key.id().to_string(),
    };
    let key_id = key.id().clone();

    let mut txn = TxnBuilder::new();
    let provenance = admitted.memory().provenance.clone();
    MemoryRepo::new()
        .insert(&mut txn, &admitted, &provenance, &common::work(memory))
        .expect("the quarantined row stages");
    aicortex_store::DecisionKeyRepo::stage(
        &mut txn,
        &memory.scope,
        &key,
        Some(memory.id),
        memory.created,
    );
    node.handle()
        .txn(txn.into_statements())
        .await
        .expect("the quarantine commits");

    let entry = aicortex_gate::ledger_entry(
        KIND_QUARANTINE,
        &reason,
        &memory.scope,
        &memory.scope.owner,
        &digest,
        Some(memory.id),
    );
    append(node, &entry, &format!("{KIND_QUARANTINE}-{}", memory.id)).await;

    Quarantined {
        memory: memory.id,
        key_id,
        digest,
        material,
    }
}

/// Stage the key of a refusal, which stores no row at all (013 B-9).
async fn refusal_key(
    node: &common::Node,
    scope: &Scope,
    at: UnixSeconds,
) -> aicortex_store::DecisionKeyId {
    let gate = Gate::standard();
    let memory = common::memory(scope, &format!("ghp_{}", "x".repeat(36)), at.get());
    let candidate = Candidate::new(common::parts_of(&memory), Origin::Asserted);
    let Verdict::Refuse(reason) = gate.evaluate(&candidate) else {
        panic!("the credential fixture must be refused");
    };
    let key = aicortex_store::DecisionKey::mint().expect("a key mints");
    let key_id = key.id().clone();
    let material = gate.digest_material(&candidate);
    let digest = DigestRef {
        digest: key.digest(&material),
        algorithm: aicortex_store::DIGEST_ALGORITHM.to_owned(),
        key_id: key_id.to_string(),
    };
    let mut txn = TxnBuilder::new();
    aicortex_store::DecisionKeyRepo::stage(&mut txn, scope, &key, None, at);
    node.handle()
        .txn(txn.into_statements())
        .await
        .expect("the refusal key commits");
    let entry = aicortex_gate::ledger_entry(
        aicortex_gate::KIND_REFUSE,
        &reason,
        scope,
        &scope.owner,
        &digest,
        None,
    );
    let decision_id = format!("refusal-{key_id}");
    append(node, &entry, &decision_id).await;
    assert!(
        record_json(node, &decision_id)
            .await
            .contains(key_id.as_str())
    );
    let stored = aicortex_store::DecisionKeyRepo::get(&node.handle(), scope, &key_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.digest(&material), digest.digest);
    key_id
}

/// Append one gate Decision to the chain.
async fn append(node: &common::Node, entry: &aicortex_gate::LedgerEntry, id: &str) {
    node.ledger
        .append(
            rahi_ledger::Decision::new(
                rahi_ledger::DecisionId::new(id.to_owned()),
                rahi_ledger::DecisionKind::new(entry.kind),
                entry.actor.clone(),
                if entry.denied {
                    rahi_ledger::Outcome::Deny
                } else {
                    rahi_ledger::Outcome::Allow
                },
                entry.reason.clone(),
            )
            .with_payload(entry.payload.clone()),
        )
        .await
        .expect("the Decision appends");
}

/// The records the chain holds, newest last.
async fn records(node: &common::Node) -> Vec<SignedRecord> {
    node.ledger.records().await.expect("the chain reads")
}

/// The record of one decision id, as canonical JSON.
async fn record_json(node: &common::Node, id: &str) -> String {
    for record in records(node).await {
        let decision = record.decision().expect("a record decodes");
        if decision.id.as_str() == id {
            return record.to_canonical_json().expect("a record renders");
        }
    }
    panic!("the chain holds no decision {id}");
}

/// How many rows a memory has in a table.
async fn rows_for(node: &common::Node, table: &str, id: MemoryId) -> u64 {
    common::count(
        node,
        &format!("SELECT COUNT(*) AS count FROM {table} WHERE memory_id = $1"),
        vec![Value::from(id.to_string())],
    )
    .await
}

/// The authority every test erases on.
fn authority() -> Authority {
    Authority::of(common::sub("alice")).because("subject_request")
}

// ---------------------------------------------------------------- FR-003

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr003_erasure_empties_the_chunk_and_embedding_tables_and_leaves_retrieval_empty() {
    let node = common::node().await;
    let alice = common::scope("alice");
    let now = UnixSeconds::new(1_700_100_000);

    let memory = common::memory(&alice, "the spare key is under the mat", 1_700_000_000);
    capture(&node, &memory).await;
    seed_derivatives(&node, &alice, memory.id).await;

    // The positive control: before the erasure the targeted search finds it
    // and every derivative table holds its row. An "is gone" assertion over
    // rows that were never there proves nothing.
    let digest = fingerprint::of_memory(&memory);
    assert_eq!(
        MemoryRepo::new()
            .fingerprint_holder(&node.handle(), &alice, &digest)
            .await
            .expect("the read succeeds"),
        Some(memory.id),
        "before the erasure a targeted search returns the memory"
    );
    for table in [
        "chunk",
        "embedding",
        "chunk_token",
        "provenance",
        "memory_source",
    ] {
        assert_eq!(
            rows_for(&node, table, memory.id).await,
            1,
            "{table} must hold a row before the erasure, or its absence proves nothing"
        );
    }

    let erased = eraser_with_future_tables()
        .erase(
            &node.handle(),
            &node.ledger,
            &Erasure::new(&alice, memory.id, &authority(), now),
        )
        .await
        .expect("the erasure runs");

    // A direct read of the embedding and chunk tables for that id returns
    // zero rows, which is FR-003 word for word.
    for table in [
        "chunk",
        "embedding",
        "chunk_token",
        "provenance",
        "memory_source",
    ] {
        assert_eq!(
            rows_for(&node, table, memory.id).await,
            0,
            "erasure must reach {table}"
        );
    }
    assert!(
        erased.removed_derivatives >= 5,
        "every removed row is counted: {erased:?}"
    );

    // A targeted search that previously returned it returns nothing.
    assert_eq!(
        MemoryRepo::new()
            .fingerprint_holder(&node.handle(), &alice, &digest)
            .await
            .expect("the read succeeds"),
        None
    );
    let active = MemoryRepo::new()
        .list(
            &node.handle(),
            &aicortex_store::CursorKey::new([3u8; aicortex_store::CursorKey::BYTES]),
            &alice,
            &MemoryFilter::active(),
            50,
            None,
        )
        .await
        .expect("the listing reads");
    assert!(active.memories.is_empty());

    node.shutdown().await;
}

// ---------------------------------------------------------------- B-7, D-1

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b7_erasure_leaves_a_tombstone_that_carries_no_content() {
    let node = common::node().await;
    let alice = common::scope("alice");
    let now = UnixSeconds::new(1_700_100_000);

    let memory = common::memory_of_kind(
        &alice,
        "the spare key is under the mat",
        1_700_000_000,
        MemoryKind::Fact,
    );
    capture(&node, &memory).await;

    Eraser::new()
        .erase(
            &node.handle(),
            &node.ledger,
            &Erasure::new(&alice, memory.id, &authority(), now),
        )
        .await
        .expect("the erasure runs");

    // The shell is retained so a reference resolves to a tombstone rather
    // than dangling (D-1).
    let tombstone = MemoryRepo::new()
        .get(&node.handle(), &alice, memory.id)
        .await
        .expect("the read succeeds")
        .expect("the row shell is retained");
    assert_eq!(tombstone.id, memory.id);
    assert_eq!(tombstone.scope, alice);
    assert_eq!(tombstone.kind, MemoryKind::Fact);
    assert_eq!(tombstone.created, memory.created, "the timestamps are kept");
    assert_eq!(tombstone.updated, now);
    assert_eq!(tombstone.status, Status::Erased);

    // And it carries nothing about the subject.
    assert!(
        tombstone.body.text.is_empty(),
        "the body is gone, not blanked"
    );
    assert!(tombstone.body.title.is_none());
    assert!(tombstone.body.media.is_empty());
    assert!(!tombstone.actor.is_human());
    assert!(tombstone.provenance.derived_from.is_empty());
    assert_ne!(
        tombstone.provenance.source.system.as_str(),
        memory.provenance.source.system.as_str(),
        "the source the memory named does not survive its erasure"
    );

    // The fingerprint column is cleared, so the same content can be captured
    // again: a tombstone that reserved the content's identity would be a
    // record of what was forgotten.
    let recapture = common::memory(&alice, "the spare key is under the mat", 1_700_200_000);
    let id = capture(&node, &recapture).await;
    assert_eq!(id, recapture.id, "a new capture is a new row, not a merge");

    // The counters moved with the status rather than being recounted.
    let stats = Counters::stats(&node.handle(), &alice)
        .await
        .expect("the counters read");
    assert_eq!(stats.by_status.get(StatusFilter::Erased.label()), Some(&1));

    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b7_a_retry_returns_the_receipt_without_erasing_twice() {
    let node = common::node().await;
    let alice = common::scope("alice");
    let now = UnixSeconds::new(1_700_100_000);

    let memory = common::memory(&alice, "the badge number is on the card", 1_700_000_000);
    capture(&node, &memory).await;
    let eraser = Eraser::new();
    let first = eraser
        .erase(
            &node.handle(),
            &node.ledger,
            &Erasure::new(&alice, memory.id, &authority(), now),
        )
        .await
        .expect("the first erasure runs");

    let repeated = eraser
        .erase(
            &node.handle(),
            &node.ledger,
            &Erasure::new(&alice, memory.id, &authority(), now),
        )
        .await
        .expect("the receipt is replayed");
    assert_eq!(first, repeated);
    assert_eq!(
        node.ledger.count().await.unwrap(),
        2,
        "genesis and one erasure"
    );
    assert_eq!(
        MemoryRepo::new()
            .get(&node.handle(), &alice, memory.id)
            .await
            .unwrap()
            .unwrap()
            .updated,
        now
    );

    // A memory of another scope is not reachable either, which is 012 B-3
    // holding on the one path where crossing scopes would be irreversible.
    let bob = common::scope("bob");
    let error = eraser
        .erase(
            &node.handle(),
            &node.ledger,
            &Erasure::new(&bob, memory.id, &authority(), now),
        )
        .await
        .expect_err("another scope cannot erase this memory");
    assert!(error.message().contains("not an erasable row"), "{error}");

    // Only one Decision was appended, by the erasure that happened.
    let erasures = records(&node)
        .await
        .into_iter()
        .filter(|record| {
            record
                .decision()
                .is_ok_and(|decision| decision.kind.as_str() == KIND_ERASE)
        })
        .count();
    assert_eq!(erasures, 1, "a refused erasure appends nothing");

    node.shutdown().await;
}

// ---------------------------------------------------------------- FR-004

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr004_the_decision_carries_neither_the_body_nor_its_hash_preimage() {
    let node = common::node().await;
    let alice = common::scope("alice");
    let now = UnixSeconds::new(1_700_100_000);

    // A distinctive token: if any part of the body reached the chain, a
    // substring search finds it.
    let token = "zarquon-flange-7731";
    let memory = common::memory(&alice, &format!("the passphrase is {token}"), 1_700_000_000);
    capture(&node, &memory).await;
    let digest = fingerprint::of_memory(&memory);

    let erased = Eraser::new()
        .erase(
            &node.handle(),
            &node.ledger,
            &Erasure::new(&alice, memory.id, &authority(), now),
        )
        .await
        .expect("the erasure runs");

    let json = record_json(&node, erased.decision.as_str()).await;
    assert!(
        !json.contains(token),
        "the Decision must contain no substring of the erased body: {json}"
    );
    assert!(
        !json.contains("passphrase"),
        "nor any other part of it: {json}"
    );
    assert!(
        !json.contains(&digest),
        "nor the content fingerprint, which is its hash preimage: {json}"
    );

    // What it does carry is what B-7 names: the scope, the id, the authority
    // and a count of removed derivatives.
    assert!(json.contains(memory.id.to_string().as_str()));
    assert!(json.contains(common::sub("alice").as_str()));
    assert!(json.contains("derivatives_removed"));
    assert!(json.contains("subject_request"));

    // And the whole chain says nothing about the body either, which is the
    // claim constitution XIII actually needs.
    for record in records(&node).await {
        let rendered = record.to_canonical_json().expect("a record renders");
        assert!(!rendered.contains(token), "the chain leaked the body");
    }

    node.shutdown().await;
}

// ---------------------------------------------------------------- B-8

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b8_a_derivative_is_marked_rather_than_erased_unless_cascading_is_asked_for() {
    let node = common::node().await;
    let alice = common::scope("alice");
    let now = UnixSeconds::new(1_700_100_000);

    let origin = common::memory(&alice, "the quarterly numbers, in full", 1_700_000_000);
    capture(&node, &origin).await;
    let summary = common::derived_memory(
        &alice,
        "revenue rose",
        1_700_000_100,
        MemoryKind::Observation,
        vec![origin.id],
    );
    capture(&node, &summary).await;

    let erased = Eraser::new()
        .erase(
            &node.handle(),
            &node.ledger,
            &Erasure::new(&alice, origin.id, &authority(), now),
        )
        .await
        .expect("the erasure runs");
    assert_eq!(erased.marked, 1, "the derivative is marked (B-8)");
    assert!(erased.cascaded.is_empty(), "cascading was not asked for");

    // The summary is still there and still says what it said.
    let stored = MemoryRepo::new()
        .get(&node.handle(), &alice, summary.id)
        .await
        .expect("the read succeeds")
        .expect("a derivative is not erased with its origin");
    assert_eq!(stored.status, Status::Active);
    assert_eq!(stored.body.text, "revenue rose");

    // And the mark is visible where a recall trace can read it.
    assert_eq!(
        common::count(
            &node,
            "SELECT COUNT(*) AS count FROM memory WHERE id = $1 AND origin_erased = 1",
            vec![Value::from(summary.id.to_string())],
        )
        .await,
        1,
        "the derivative records that its origin is gone"
    );

    // Cascading is the explicit flag, and is reported in the Decision.
    let second_origin = common::memory(&alice, "the monthly numbers, in full", 1_700_000_200);
    capture(&node, &second_origin).await;
    let second_summary = common::derived_memory(
        &alice,
        "costs fell",
        1_700_000_300,
        MemoryKind::Observation,
        vec![second_origin.id],
    );
    capture(&node, &second_summary).await;

    let cascaded = Eraser::new()
        .erase(
            &node.handle(),
            &node.ledger,
            &Erasure::new(&alice, second_origin.id, &authority(), now).cascading(),
        )
        .await
        .expect("the cascading erasure runs");
    assert_eq!(cascaded.cascaded, vec![second_summary.id]);
    let gone = MemoryRepo::new()
        .get(&node.handle(), &alice, second_summary.id)
        .await
        .expect("the read succeeds")
        .expect("the shell is retained");
    assert_eq!(gone.status, Status::Erased);
    assert!(gone.body.text.is_empty());

    let json = record_json(&node, cascaded.decision.as_str()).await;
    assert!(json.contains("\"cascade\":true"), "{json}");
    assert!(
        json.contains(second_summary.id.to_string().as_str()),
        "{json}"
    );

    node.shutdown().await;
}

// ---------------------------------------------------------------- FR-007

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr007_erasing_a_quarantined_memory_destroys_the_key_its_decision_names() {
    let node = common::node().await;
    let alice = common::scope("alice");
    let now = UnixSeconds::new(1_700_100_000);

    let memory = common::memory(
        &alice,
        "an unattributable note about a person",
        1_700_000_000,
    );
    let quarantined = quarantine(&node, &memory).await;

    // ---- The presence, established before anything is erased. ----
    //
    // The key row exists, and it is the *right* key: recomputing the digest
    // the Decision carries, with the key read back out of the store,
    // reproduces it exactly. That is what makes the absence below a fact
    // about erasure rather than a fact about a key that was never written.
    let key = aicortex_store::DecisionKeyRepo::get(&node.handle(), &alice, &quarantined.key_id)
        .await
        .expect("the read succeeds")
        .expect("the Decision's key is in the store before the erasure");
    assert_eq!(
        key.digest(&quarantined.material),
        quarantined.digest.digest,
        "the stored key reproduces the Decision's digest, so it is the key that Decision names"
    );
    assert_eq!(
        common::count(
            &node,
            "SELECT COUNT(*) AS count FROM decision_key WHERE memory_id = $1",
            vec![Value::from(quarantined.memory.to_string())],
        )
        .await,
        1
    );
    // The Decision naming it really is in the chain.
    let quarantine_json = record_json(&node, &format!("{KIND_QUARANTINE}-{}", memory.id)).await;
    assert!(quarantine_json.contains(quarantined.key_id.as_str()));

    // ---- The erasure. ----
    let erased = Eraser::new()
        .erase(
            &node.handle(),
            &node.ledger,
            &Erasure::new(&alice, quarantined.memory, &authority(), now),
        )
        .await
        .expect("the erasure runs");
    assert_eq!(erased.keys_destroyed, 1, "B-11: the key went with the row");

    // ---- The absence. ----
    assert_eq!(
        aicortex_store::DecisionKeyRepo::get(&node.handle(), &alice, &quarantined.key_id)
            .await
            .expect("the read succeeds"),
        None,
        "FR-007: DecisionKeyRepo::get returns None for the key id the Decision names"
    );
    assert_eq!(
        common::count(
            &node,
            "SELECT COUNT(*) AS count FROM decision_key WHERE key_id = $1",
            vec![Value::from(quarantined.key_id.as_str())],
        )
        .await,
        0,
        "FR-007: no decision_key row covering the erased object remains"
    );

    // ---- No replay path restores one. ----
    //
    // Redriving the outbox: the work the quarantine staged is drained, which
    // is the retry path spec 015's worker will run.
    Outbox::drain(&node.handle(), 100)
        .await
        .expect("the outbox drains");
    // Re-importing the same content: a fresh capture of the same body.
    let reimport = common::memory(
        &alice,
        "an unattributable note about a person",
        1_700_300_000,
    );
    capture(&node, &reimport).await;
    // Quarantining the exact erased body also mints a new key. First erase
    // the admitted recapture so the original scope can
    // exercise an exact-body quarantine without violating uniqueness.
    Eraser::new()
        .erase(
            &node.handle(),
            &node.ledger,
            &Erasure::new(
                &alice,
                reimport.id,
                &authority(),
                UnixSeconds::new(1_700_350_000),
            ),
        )
        .await
        .expect("the recapture erases");
    let second = quarantine(
        &node,
        &common::memory(
            &alice,
            "an unattributable note about a person",
            1_700_400_000,
        ),
    )
    .await;
    assert_ne!(second.key_id, quarantined.key_id);

    assert_eq!(
        aicortex_store::DecisionKeyRepo::get(&node.handle(), &alice, &quarantined.key_id)
            .await
            .expect("the read succeeds"),
        None,
        "FR-007: no replay mints a row under the destroyed key id"
    );
    assert_eq!(
        common::count(
            &node,
            "SELECT COUNT(*) AS count FROM decision_key WHERE key_id = $1",
            vec![Value::from(quarantined.key_id.as_str())],
        )
        .await,
        0
    );

    // ---- And the honest caveat is said in so many words. ----
    let caveat = Erased::caveat();
    assert!(
        caveat.contains("before") && caveat.contains("backup"),
        "FR-007 requires the result to say a backup taken before the erasure still holds the key: \
         {caveat}"
    );
    let json = record_json(&node, erased.decision.as_str()).await;
    assert!(json.contains("caveat"), "and to carry it into the Decision");

    node.shutdown().await;
}

/// The key material of a quarantined capture, read straight out of the table.
///
/// The *secret*, not the key id. Scanning a backup for the id would say
/// nothing: the id is in the decision chain by design, so a backup that holds
/// the chain holds the id whether or not the key survived. The secret is the
/// thing that makes a chained digest confirmable, and it is the thing an
/// erasure has to take away.
async fn key_secret(node: &common::Node, key_id: &aicortex_store::DecisionKeyId) -> Vec<u8> {
    #[derive(serde::Deserialize)]
    struct SecretRow {
        secret: Vec<u8>,
    }
    let rows: Vec<SecretRow> = node
        .handle()
        .query_consistent(
            "SELECT secret FROM decision_key WHERE key_id = $1",
            vec![Value::from(key_id.as_str())],
        )
        .await
        .expect("the secret reads");
    let secret = rows.into_iter().next().expect("the key is there").secret;
    assert_eq!(secret.len(), 32, "a minted key is 32 bytes");
    secret
}

/// The positive control for the test below, and the caveat FR-007 requires
/// the result of an erasure to state in so many words.
///
/// Recover the actual pre-erasure snapshot after erasing its source. The key
/// must return: FR-007 requires this limitation to be stated, not hidden.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr007_a_backup_taken_before_the_erasure_still_holds_the_key() {
    let node = common::node().await;
    let alice = common::scope("alice");

    let memory = common::memory(
        &alice,
        "an unattributable note about a person",
        1_700_000_000,
    );
    let quarantined = quarantine(&node, &memory).await;
    let secret = key_secret(&node, &quarantined.key_id).await;

    let backup = backup_bytes(&node).await;
    assert!(
        contains(&backup, &secret),
        "a backup taken before the erasure holds the key, which is exactly what \
         Erased::caveat() says it does"
    );

    Eraser::new()
        .erase(
            &node.handle(),
            &node.ledger,
            &Erasure::new(
                &alice,
                memory.id,
                &authority(),
                UnixSeconds::new(1_700_100_000),
            ),
        )
        .await
        .expect("the source is erased after its backup");
    assert_eq!(
        aicortex_store::DecisionKeyRepo::get(&node.handle(), &alice, &quarantined.key_id)
            .await
            .unwrap(),
        None
    );
    let recovered = common::recover_snapshot(&backup).await;
    let key =
        aicortex_store::DecisionKeyRepo::get(&recovered.handle(), &alice, &quarantined.key_id)
            .await
            .unwrap()
            .expect("a pre-erasure backup restores the old key");
    assert_eq!(key.digest(&quarantined.material), quarantined.digest.digest);
    recovered
        .ledger
        .verify()
        .await
        .expect("the recovered chain verifies");
    recovered.shutdown().await;

    // And the caveat says so, rather than leaving somebody to find out.
    let caveat = Erased::caveat();
    assert!(caveat.contains("backup"), "{caveat}");
    assert!(caveat.contains("before"), "{caveat}");
    assert!(caveat.contains("discarded"), "{caveat}");

    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr007_a_backup_taken_after_the_erasure_holds_no_key() {
    let node = common::node().await;
    let alice = common::scope("alice");
    let now = UnixSeconds::new(1_700_100_000);

    let memory = common::memory(
        &alice,
        "an unattributable note about a person",
        1_700_000_000,
    );
    let quarantined = quarantine(&node, &memory).await;
    let secret = key_secret(&node, &quarantined.key_id).await;

    Eraser::new()
        .erase(
            &node.handle(),
            &node.ledger,
            &Erasure::new(&alice, quarantined.memory, &authority(), now),
        )
        .await
        .expect("the erasure runs");

    let backup = backup_bytes(&node).await;
    assert!(
        !contains(&backup, &secret),
        "FR-007: a backup taken after the erasure holds no such key"
    );
    // The backup is a real snapshot of this store and not an empty file: it
    // holds the tombstone's id, so the absence above is an absence of the key
    // rather than an absence of everything.
    assert!(
        contains(&backup, quarantined.memory.to_string().as_bytes()),
        "the snapshot really is this store's"
    );

    let recovered = common::recover_snapshot(&backup).await;
    let tombstone = MemoryRepo::new()
        .get(&recovered.handle(), &alice, memory.id)
        .await
        .unwrap()
        .expect("the post-erasure snapshot restores the tombstone");
    assert_eq!(tombstone.status, Status::Erased);
    assert_eq!(
        aicortex_store::DecisionKeyRepo::get(&recovered.handle(), &alice, &quarantined.key_id)
            .await
            .unwrap(),
        None
    );
    Outbox::drain(&recovered.handle(), 100)
        .await
        .expect("recovered work redrives");
    let recaptured = quarantine(
        &recovered,
        &common::memory(
            &alice,
            "an unattributable note about a person",
            1_700_400_000,
        ),
    )
    .await;
    assert_ne!(recaptured.key_id, quarantined.key_id);
    assert_eq!(
        aicortex_store::DecisionKeyRepo::get(&recovered.handle(), &alice, &quarantined.key_id)
            .await
            .unwrap(),
        None
    );
    recovered
        .ledger
        .verify()
        .await
        .expect("the recovered chain verifies after replay");
    recovered.shutdown().await;

    node.shutdown().await;
}

/// Resolve once the scope holds at least `at_least` tombstones, and fewer
/// than `below`.
///
/// Polled rather than timed, so the moment a scope erasure is dropped is a
/// fact about the work it had done and not about how fast the machine is.
async fn erased_reaches(node: &common::Node, scope: &Scope, at_least: u64, below: u64) -> u64 {
    loop {
        let erased = common::count(
            node,
            "SELECT COUNT(*) AS count FROM memory WHERE scope_id = $1 AND status = 'erased'",
            vec![Value::from(&ScopeId::of(scope))],
        )
        .await;
        if erased >= at_least && erased < below {
            return erased;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

/// Whether `haystack` contains `needle` as a contiguous byte sequence.
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr007_erasing_a_scope_destroys_every_key_in_it_including_a_refusal_s() {
    let node = common::node().await;
    let alice = common::scope("alice");
    let now = UnixSeconds::new(1_700_100_000);

    let memory = common::memory(
        &alice,
        "an unattributable note about a person",
        1_700_000_000,
    );
    let quarantined = quarantine(&node, &memory).await;
    // A refusal stores no row at all, so its key is reachable only through
    // the scope. It is the case a per-memory sweep would miss entirely.
    let refused = refusal_key(&node, &alice, UnixSeconds::new(1_700_000_100)).await;
    let bob = common::scope("bob");
    let bobs_key = refusal_key(&node, &bob, UnixSeconds::new(1_700_000_200)).await;

    // The presence, established first.
    for key_id in [&quarantined.key_id, &refused] {
        assert!(
            aicortex_store::DecisionKeyRepo::get(&node.handle(), &alice, key_id)
                .await
                .expect("the read succeeds")
                .is_some(),
            "the key must be there before the erasure, or its absence proves nothing"
        );
    }
    assert_eq!(
        common::count(
            &node,
            "SELECT COUNT(*) AS count FROM decision_key WHERE scope_id = $1",
            vec![Value::from(&ScopeId::of(&alice))],
        )
        .await,
        2
    );

    let erased = Eraser::new()
        .erase_scope(&node.handle(), &node.ledger, &alice, &authority(), 100, now)
        .await
        .expect("the scope erasure runs");
    assert_eq!(erased.memories, 1);
    assert_eq!(erased.keys_destroyed, 2, "{erased:?}");

    // The absence, for both kinds of key.
    for key_id in [&quarantined.key_id, &refused] {
        assert_eq!(
            aicortex_store::DecisionKeyRepo::get(&node.handle(), &alice, key_id)
                .await
                .expect("the read succeeds"),
            None,
            "erasing a scope destroys every key in it (B-11)"
        );
    }
    assert_eq!(
        common::count(
            &node,
            "SELECT COUNT(*) AS count FROM decision_key WHERE scope_id = $1",
            vec![Value::from(&ScopeId::of(&alice))],
        )
        .await,
        0
    );

    // A scope that holds only refusals still has its keys destroyed.
    assert!(
        aicortex_store::DecisionKeyRepo::get(&node.handle(), &bob, &bobs_key)
            .await
            .expect("the read succeeds")
            .is_some()
    );
    let erased = Eraser::new()
        .erase_scope(
            &node.handle(),
            &node.ledger,
            &bob,
            &Authority::of(common::sub("bob")),
            100,
            now,
        )
        .await
        .expect("a scope of refusals erases");
    assert_eq!(erased.keys_destroyed, 1);
    assert_eq!(erased.memories, 0);
    assert_eq!(erased.removed_derivatives, 1);
    assert_eq!(erased.batches, 1, "the key-only batch is journaled");
    let json = record_json(&node, erased.decision.as_str()).await;
    assert!(json.contains("\"keys_destroyed\":1"), "{json}");
    assert!(json.contains("\"derivatives_removed\":1"), "{json}");
    assert_eq!(
        aicortex_store::DecisionKeyRepo::get(&node.handle(), &bob, &bobs_key)
            .await
            .expect("the read succeeds"),
        None
    );

    let backup = backup_bytes(&node).await;
    let recovered = common::recover_snapshot(&backup).await;
    for (scope, key_id) in [
        (&alice, &quarantined.key_id),
        (&alice, &refused),
        (&bob, &bobs_key),
    ] {
        assert_eq!(
            aicortex_store::DecisionKeyRepo::get(&recovered.handle(), scope, key_id)
                .await
                .unwrap(),
            None
        );
    }
    Outbox::drain(&recovered.handle(), 100).await.unwrap();
    let replay = refusal_key(&recovered, &bob, UnixSeconds::new(1_700_400_000)).await;
    assert_ne!(replay, bobs_key);
    assert_eq!(
        aicortex_store::DecisionKeyRepo::get(&recovered.handle(), &bob, &bobs_key)
            .await
            .unwrap(),
        None
    );
    recovered.ledger.verify().await.unwrap();
    recovered.shutdown().await;

    node.shutdown().await;
}

// ---------------------------------------------------------------- FR-005

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn h3_restart_with_a_held_lease_finishes_once_from_a_fresh_request() {
    let mut node = common::node().await;
    let scope = common::scope("restart-held-lease");
    let memory = common::memory(&scope, "erase once after restart", 1_700_000_000);
    capture(&node, &memory).await;
    let key = erasure_lease_key(&ScopeId::of(&scope));
    let held = node
        .handle()
        .lease(&key)
        .await
        .expect("the lease is held at stop");

    node = node.reopen_after_stop_with_lease(held).await;
    tokio::time::sleep(std::time::Duration::from_secs(LEASE_TTL_SECONDS + 1)).await;

    // This request is created after the wait. A request queued before expiry
    // is not reused, because hiqlite expires a dead holder on a fresh request.
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        Eraser::new().erase_scope(
            &node.handle(),
            &node.ledger,
            &scope,
            &authority(),
            2,
            AicortexTime::new(1_700_100_000),
        ),
    )
    .await
    .expect("the fresh request after TTL does not hang")
    .expect("the restarted operation completes");
    assert_eq!((result.batches, result.memories), (1, 1));
    assert_eq!(
        MemoryRepo::new()
            .get(&node.handle(), &scope, memory.id)
            .await
            .expect("the tombstone reads")
            .expect("the tombstone remains")
            .status,
        Status::Erased
    );
    assert_eq!(node.ledger.count().await.expect("the chain count"), 2);

    let repeated = Eraser::new()
        .erase_scope(
            &node.handle(),
            &node.ledger,
            &scope,
            &authority(),
            2,
            AicortexTime::new(1_700_200_000),
        )
        .await
        .expect("the completed operation is replayed");
    assert_eq!(repeated, result, "the operation is finished exactly once");
    assert_eq!(node.ledger.count().await.expect("the chain count"), 2);
    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr005_a_scope_erasure_of_five_thousand_memories_is_bounded_and_resumable() {
    let node = common::node().await;
    let alice = common::scope("alice");
    let now = UnixSeconds::new(1_700_100_000);
    let total = 5000_usize;

    // Seed the scope. The captures go through the gate and the repository, so
    // these are real rows with real provenance and real source log entries.
    let mut ids = Vec::with_capacity(total);
    for chunk in 0..(total / 100) {
        let mut txn = TxnBuilder::new();
        for index in 0..100 {
            let memory = common::memory(
                &alice,
                &format!("note number {}", chunk * 100 + index),
                1_700_000_000 + (chunk * 100 + index) as u64,
            );
            let admitted = common::admit(&memory);
            let provenance = memory.provenance.clone();
            MemoryRepo::new()
                .insert(&mut txn, &admitted, &provenance, &common::work(&memory))
                .expect("the row stages");
            ids.push(memory.id);
        }
        node.handle()
            .txn(txn.into_statements())
            .await
            .expect("the seed commits");
    }
    assert_eq!(ids.len(), total);
    // Derivatives that must not be orphaned, on the first and the last row.
    seed_derivatives(&node, &alice, ids[0]).await;
    seed_derivatives(&node, &alice, ids[total - 1]).await;

    let eraser = eraser_with_future_tables();

    // The induced crash: the pass is dropped mid-flight, part way through.
    // A dropped future is exactly what a process that dies does to the work it
    // was running, and surviving it is what "resumable" has to mean.
    //
    // The drop is triggered by observed progress rather than by a stopwatch. A
    // fixed delay would be a race on a slower machine, where the window can
    // close before the first batch commits and the test would then be
    // asserting about an interruption that interrupted nothing.
    // `select!` borrows its branches, so the handle and the authority are
    // bound rather than built inline: a temporary would be dropped at the end
    // of the statement that borrows it.
    let handle = node.handle();
    let who = authority();
    let interrupted = tokio::select! {
        finished = eraser.erase_scope(&handle, &node.ledger, &alice, &who, 25, now)
            => Some(finished),
        partial = erased_reaches(&node, &alice, 50, total as u64) => {
            assert!(partial >= 50, "the pass made no progress before it was dropped");
            None
        }
    };
    assert!(
        interrupted.is_none(),
        "the pass was meant to be dropped part way through, not to finish"
    );
    let partial = common::count(
        &node,
        "SELECT COUNT(*) AS count FROM memory WHERE scope_id = $1 AND status = 'erased'",
        vec![Value::from(&ScopeId::of(&alice))],
    )
    .await;
    assert!(
        partial > 0 && partial < total as u64,
        "the interruption must leave the scope part way through, not untouched and not \
         finished: {partial} of {total}"
    );

    // Whatever the interruption left, a rerun completes it.
    let erased = eraser
        .erase_scope(
            &node.handle(),
            &node.ledger,
            &alice,
            &authority(),
            MAX_ERASURE_BATCH,
            now,
        )
        .await
        .expect("the resumed erasure completes");
    assert!(erased.batches >= 1, "{erased:?}");
    assert_eq!(erased.memories, total as u64);
    assert_eq!(erased.removed_derivatives, total as u64 + 6);
    assert_eq!(erased.keys_destroyed, 0);

    // Nothing is left un-erased.
    assert_eq!(
        common::count(
            &node,
            "SELECT COUNT(*) AS count FROM memory WHERE scope_id = $1 AND status <> 'erased'",
            vec![Value::from(&ScopeId::of(&alice))],
        )
        .await,
        0,
        "a resumed scope erasure finishes the job"
    );
    // And the shells are all still there: erasure is not a delete (D-1).
    assert_eq!(
        common::count(
            &node,
            "SELECT COUNT(*) AS count FROM memory WHERE scope_id = $1",
            vec![Value::from(&ScopeId::of(&alice))],
        )
        .await,
        total as u64
    );

    // No orphaned embedding or chunk row, which is FR-005's own words.
    for table in [
        "chunk",
        "embedding",
        "chunk_token",
        "provenance",
        "memory_source",
    ] {
        assert_eq!(
            common::count(
                &node,
                &format!("SELECT COUNT(*) AS count FROM {table} WHERE scope_id = $1"),
                vec![Value::from(&ScopeId::of(&alice))],
            )
            .await,
            0,
            "{table} must hold no orphan after a scope erasure"
        );
    }

    // Bounded: the journal records more than one batch, which is the only
    // way 5000 memories could have been claimed at 500 a time.
    let batches = common::count(
        &node,
        "SELECT COUNT(*) AS count FROM erasure_journal WHERE scope_id = $1",
        vec![Value::from(&ScopeId::of(&alice))],
    )
    .await;
    assert!(
        batches >= (total as u64) / u64::from(MAX_ERASURE_BATCH),
        "the work was done in bounded batches, not one transaction: {batches}"
    );

    // One Decision at completion, not one per batch.
    let completions = records(&node)
        .await
        .into_iter()
        .filter(|record| {
            record
                .decision()
                .is_ok_and(|decision| decision.kind.as_str() == KIND_ERASE_SCOPE)
        })
        .count();
    assert_eq!(completions, 1, "B-9: a single Decision at completion");

    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b9_an_unbounded_erasure_batch_is_refused() {
    let node = common::node().await;
    let alice = common::scope("alice");
    let now = UnixSeconds::new(1_700_100_000);

    for batch in [0, MAX_ERASURE_BATCH + 1] {
        let error = Eraser::new()
            .erase_scope(
                &node.handle(),
                &node.ledger,
                &alice,
                &authority(),
                batch,
                now,
            )
            .await
            .expect_err("an out-of-range batch is refused");
        assert!(error.message().contains("is not between 1 and"), "{error}");
    }

    node.shutdown().await;
}

// ---------------------------------------------------------------- AC-2

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ac2_the_chain_still_verifies_after_an_erasure() {
    let node = common::node().await;
    let alice = common::scope("alice");
    let now = UnixSeconds::new(1_700_100_000);

    let memory = common::memory(&alice, "an unattributable note", 1_700_000_000);
    let quarantined = quarantine(&node, &memory).await;
    let refused = refusal_key(&node, &alice, UnixSeconds::new(1_700_000_100)).await;
    let _ = refused;

    let before = node.ledger.head().await.expect("the head reads");
    node.ledger
        .verify()
        .await
        .expect("the chain verifies first");

    Eraser::new()
        .erase(
            &node.handle(),
            &node.ledger,
            &Erasure::new(&alice, quarantined.memory, &authority(), now),
        )
        .await
        .expect("the erasure runs");

    // `Ledger::verify` is `verify_chain(Depth::Resident)`, which is exactly
    // what the chassis's `aicortex ledger verify` verb runs (`rahi_cli`).
    node.ledger
        .verify()
        .await
        .expect("AC-2: the chain is intact after an erasure");
    let after = node.ledger.head().await.expect("the head reads");
    assert_ne!(before, after, "the erasure appended to the chain");

    // The quarantine Decision is still in the chain, unaltered: erasure
    // destroys the key, never a record. What it costs is the ability to
    // confirm the digest, which is the whole of the bargain.
    let json = record_json(&node, &format!("{KIND_QUARANTINE}-{}", memory.id)).await;
    assert!(json.contains(quarantined.digest.digest.as_str()));
    assert!(json.contains(quarantined.key_id.as_str()));
    assert_eq!(
        aicortex_store::DecisionKeyRepo::get(&node.handle(), &alice, &quarantined.key_id)
            .await
            .expect("the read succeeds"),
        None,
        "the digest is in the chain forever and can no longer be confirmed"
    );

    node.shutdown().await;
}

// ---------------------------------------------------------------- the sweep

#[test]
fn the_sweep_names_every_table_a_later_spec_will_add() {
    // A guard against the quiet failure mode of `PLANNED`: an entry that gets
    // dropped rather than moved, so a table specs 015 and 016 create is never
    // swept and B-7's frozen invariant fails silently a spec later.
    let tables: Vec<&str> = PLANNED.iter().map(|planned| planned.table).collect();
    assert!(tables.contains(&"chunk"), "015's chunks (B-7)");
    assert!(tables.contains(&"embedding"), "015's vectors (B-7)");
    assert!(tables.contains(&"chunk_token"), "016's index entries (B-7)");
    for planned in PLANNED {
        assert!(
            !DERIVATIVES.contains(planned),
            "{} is in both lists: it was copied rather than moved",
            planned.table
        );
    }
    // And registering one is what makes it swept.
    let eraser = Eraser::new().also(Derivative::new("chunk", "memory_id", "scope_id"));
    assert_eq!(eraser.derivatives().len(), DERIVATIVES.len() + 1);
    // Twice is once: a registry that grew on every call would emit a DELETE
    // per registration and count the same removal more than once.
    let eraser = eraser.also(Derivative::new("chunk", "memory_id", "scope_id"));
    assert_eq!(eraser.derivatives().len(), DERIVATIVES.len() + 1);
}

/// Run explicitly after building the current cell binary. This invokes the
/// actual chassis CLI against a real erasure, with the cell's own genesis.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires AICORTEX_VERIFY_BINARY built from the current tree"]
async fn ac2_literal_cli_verifies_a_real_erased_fixture() {
    use base64::Engine as _;
    use std::os::unix::fs::PermissionsExt;
    let binary = std::env::var("AICORTEX_VERIFY_BINARY").expect("the current cell binary");
    let fixture = common::Fixture::migrated_for_cell().await;
    let cfg = fixture.store.config().clone();
    let keys = fixture.dir.path().join("keys");
    std::fs::create_dir(&keys).unwrap();
    std::fs::set_permissions(&keys, std::fs::Permissions::from_mode(0o700)).unwrap();
    let files = [
        (
            "ledger.key",
            base64::engine::general_purpose::STANDARD
                .encode([7u8; 32])
                .into_bytes(),
        ),
        ("hiqlite.json", serde_json::to_vec(&cfg.secrets).unwrap()),
        ("session.key", vec![8u8; 32]),
        ("backup.key", b"unused-by-ledger-verification".to_vec()),
        (
            "rauthy_admin_token",
            b"unused-by-ledger-verification".to_vec(),
        ),
    ];
    for (name, bytes) in files {
        let path = keys.join(name);
        std::fs::write(&path, bytes).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let verify = || {
        std::process::Command::new(&binary)
            .args(["ledger", "verify"])
            .env("RAHI_PUBLIC_URL", "http://localhost:8080")
            .env("RAHI_DATA_DIR", fixture.dir.path())
            .env("RAHI_HIQLITE_RAFT_ADDR", cfg.raft_addr.to_string())
            .env("RAHI_HIQLITE_API_ADDR", cfg.api_addr.to_string())
            .output()
            .unwrap()
    };
    fixture.store.shutdown().await.unwrap();
    drop(fixture.store);
    let initial = verify();
    assert!(
        initial.status.success(),
        "{}",
        String::from_utf8_lossy(&initial.stderr)
    );
    let store = rahi_store::Store::open(&cfg).await.unwrap();
    #[derive(serde::Deserialize)]
    struct Genesis {
        record: Vec<u8>,
    }
    let rows: Vec<Genesis> = store
        .handle()
        .query_consistent("SELECT record FROM kernel_decisions", vec![])
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    let genesis = rahi_ledger::SignedRecord::from_bytes(&rows[0].record)
        .unwrap()
        .decision()
        .unwrap();
    let ledger = rahi_ledger::Ledger::open(
        store.handle(),
        rahi_ledger::LedgerSigner::from_seed([7u8; 32]),
        genesis.prev_hash,
    )
    .await
    .unwrap();
    let scope = common::scope("cli-fixture");
    let memory = common::memory(&scope, "distinctive CLI erasure fixture", 1_700_000_000);
    let mut txn = TxnBuilder::new();
    MemoryRepo::new()
        .insert(
            &mut txn,
            &common::admit(&memory),
            &memory.provenance,
            &common::work(&memory),
        )
        .unwrap();
    store.handle().txn(txn.into_statements()).await.unwrap();
    Eraser::new()
        .erase(
            &store.handle(),
            &ledger,
            &Erasure::new(
                &scope,
                memory.id,
                &Authority::of(scope.owner.clone()),
                UnixSeconds::new(1_700_100_000),
            ),
        )
        .await
        .unwrap();
    assert_eq!(
        MemoryRepo::new()
            .get(&store.handle(), &scope, memory.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        Status::Erased
    );
    assert_eq!(ledger.count().await.unwrap(), 2);
    drop(ledger);
    store.shutdown().await.unwrap();
    drop(store);
    let result = verify();
    let stdout = String::from_utf8_lossy(&result.stdout);
    println!(
        "aicortex ledger verify: exit {:?}\n{stdout}",
        result.status.code()
    );
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(stdout.contains("2 resident record(s)"), "{stdout}");
}
