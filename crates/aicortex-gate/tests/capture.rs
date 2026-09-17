//! The composition: gate, store and ledger together (spec 013 AC-2, FR-003,
//! FR-006, FR-007, B-9).
//!
//! `cargo test -p aicortex-gate` is the command AC-1 names, and AC-2 is a
//! claim about a *capture*, not about the gate: that a fixture carrying an API
//! key is refused end to end, that the store holds no row afterwards, and that
//! the ledger holds one Decision. So the capture path is here, written once and
//! exercised by every test in this file. It is what the surface of spec 020
//! will call; until that surface exists this is the only composition, and a
//! test that reimplemented it would be asserting about code nothing runs.

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

use aicortex_gate::{
    Candidate, DigestRef, Gate, KIND_OVERRIDE, KIND_QUARANTINE, KIND_REFUSE, LedgerEntry, Override,
    Reason, Verdict, ledger_entry,
};
use aicortex_store::{DIGEST_ALGORITHM, DecisionKey, DecisionKeyId, DecisionKeyRepo, MemoryRepo};
use aicortex_types::{DecisionRef, MemoryId, Status};
use rahi_ledger::{Decision, DecisionId, DecisionKind, Outcome, SignedRecord};
use rahi_store::{Envelope, TxnBuilder};
use rahi_types::{Error, Revision, Sub};

mod capture {
    use super::*;

    /// What one capture did.
    #[derive(Debug)]
    pub struct Captured {
        /// The verdict the gate returned.
        pub verdict: Verdict,
        /// The Decision's id, when one was appended.
        pub decision: Option<String>,
        /// The digest the Decision carries.
        pub digest: Option<DigestRef>,
    }

    /// Offer `candidate` to the gate and do exactly what the verdict says.
    ///
    /// The one transaction is the point (constitution XI): a quarantine stages
    /// the memory, its provenance, its counter, its outbox work *and* the
    /// Decision's digest key together, so a stored row never exists without
    /// the key that covers it, and neither exists if the commit fails. A
    /// refusal stages the key alone, because it stores no row.
    pub async fn capture(
        node: &common::Node,
        gate: &Gate,
        candidate: &Candidate,
        actor: &Sub,
        over: Option<&Override>,
    ) -> Result<Captured, Error> {
        let verdict = match over {
            Some(over) => gate
                .evaluate_overridden(candidate, over)
                .map_err(|error| Error::Validation(error.to_string()))?,
            None => gate.evaluate(candidate),
        };
        let mut txn = TxnBuilder::new();

        if let Some(admitted) = verdict.admitted() {
            let memory = admitted.memory();
            let provenance = memory.provenance.clone();
            let work = Envelope::new(
                "memory",
                Some(memory.scope.owner.as_str().to_owned()),
                memory.id.to_string(),
                Revision::new(1),
            );
            MemoryRepo::new().insert(&mut txn, admitted, &provenance, &work)?;
        }

        // B-9: a refusal and a quarantine are ledgered; an admission is not.
        let Some(reason) = verdict.reason() else {
            node.handle().txn(txn.into_statements()).await?;
            return Ok(Captured {
                verdict,
                decision: None,
                digest: None,
            });
        };

        let key = DecisionKey::mint()?;
        let digest = DigestRef {
            digest: key.digest(&gate.digest_material(candidate)),
            algorithm: DIGEST_ALGORITHM.to_owned(),
            key_id: key.id().to_string(),
        };
        let stored = verdict.admitted().map(|admitted| admitted.memory().id);
        DecisionKeyRepo::stage(
            &mut txn,
            &candidate.parts.scope,
            &key,
            stored,
            candidate.parts.created,
        );
        node.handle().txn(txn.into_statements()).await?;

        let kind = if matches!(verdict, Verdict::Refuse(_)) {
            KIND_REFUSE
        } else {
            KIND_QUARANTINE
        };
        let entry = ledger_entry(kind, reason, &candidate.parts.scope, actor, &digest, stored);
        let id = append(node, &entry, candidate.parts.id).await?;
        Ok(Captured {
            verdict,
            decision: Some(id),
            digest: Some(digest),
        })
    }

    /// Append one entry, with an id derived from the candidate so a replay is
    /// visible rather than silent.
    pub async fn append(
        node: &common::Node,
        entry: &LedgerEntry,
        candidate: MemoryId,
    ) -> Result<String, Error> {
        let id = format!("{}-{}", entry.kind, candidate);
        let outcome = if entry.denied {
            Outcome::Deny
        } else {
            Outcome::Allow
        };
        node.ledger
            .append(
                Decision::new(
                    DecisionId::new(id.clone()),
                    DecisionKind::new(entry.kind),
                    entry.actor.clone(),
                    outcome,
                    entry.reason.clone(),
                )
                .with_payload(entry.payload.clone()),
            )
            .await?;
        Ok(id)
    }
}

use capture::capture;

/// The fixture the acceptance criterion names: a note with an API key in it.
fn fixture(name: &str) -> common::Fixture {
    common::corpus()
        .into_iter()
        .find(|fixture| fixture.name == name)
        .unwrap_or_else(|| panic!("{name} is not in the corpus"))
}

/// How many rows a table holds.
async fn rows(node: &common::Node, table: &str) -> u64 {
    #[derive(serde::Deserialize)]
    struct Count {
        count: i64,
    }
    let rows: Vec<Count> = node
        .handle()
        .query_consistent(format!("SELECT COUNT(*) AS count FROM {table}"), vec![])
        .await
        .expect("a count reads");
    u64::try_from(rows.first().map_or(0, |row| row.count)).expect("a non-negative count")
}

/// The records the chain holds, newest last.
async fn records(node: &common::Node) -> Vec<SignedRecord> {
    node.ledger.records().await.expect("the chain reads")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ac2_a_capture_carrying_an_api_key_is_refused_end_to_end() {
    let node = common::node().await;
    let fixture = fixture("secret-openai-api-key");
    let candidate = fixture.candidate();
    let operator = common::sub("capture");
    let before = records(&node).await.len();

    let outcome = capture(&node, &fixture.gate(), &candidate, &operator, None)
        .await
        .expect("the capture path runs");

    // Refused.
    let Verdict::Refuse(Reason::SecretDetected { detector, .. }) = &outcome.verdict else {
        panic!("the gate did not refuse an API key: {:?}", outcome.verdict);
    };
    assert_eq!(detector.as_str(), "openai-api-key");

    // The store holds no row: not the memory, not its scope, not its
    // provenance, not the counter, not the outbox work.
    for table in [
        "memory",
        "provenance",
        "memory_derivation",
        "scope",
        "scope_counter",
        "outbox",
    ] {
        assert_eq!(
            rows(&node, table).await,
            0,
            "a refused capture left a row in {table}"
        );
    }

    // The ledger holds one Decision, and it is this refusal.
    let after = records(&node).await;
    assert_eq!(
        after.len() - before,
        1,
        "a refused capture appended {} decisions",
        after.len() - before
    );
    let decision = after
        .last()
        .expect("a record")
        .decision()
        .expect("a decision");
    assert_eq!(decision.kind.as_str(), KIND_REFUSE);
    assert_eq!(decision.outcome, Outcome::Deny);
    assert_eq!(&decision.actor, &operator);

    // One key row, for that Decision, holding no content.
    assert_eq!(rows(&node, "decision_key").await, 1);
    assert!(
        node.ledger.verify().await.is_ok(),
        "the chain does not verify"
    );

    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr006_the_decision_carries_a_keyed_digest_and_no_hash_of_the_body() {
    let node = common::node().await;
    let fixture = fixture("secret-anthropic-api-key");
    let candidate = fixture.candidate();
    let gate = fixture.gate();
    let operator = common::sub("auditor");

    let outcome = capture(&node, &gate, &candidate, &operator, None)
        .await
        .expect("the capture path runs");
    let digest = outcome.digest.as_ref().expect("a refusal digests");
    assert_eq!(digest.algorithm, DIGEST_ALGORITHM);
    assert_eq!(
        digest.digest.len(),
        64,
        "an HMAC-SHA-256 is 32 bytes of hex"
    );

    let record = records(&node).await.pop().expect("a record");
    let serialized = record.to_canonical_json().expect("a record serializes");
    assert!(
        serialized.contains(&digest.digest),
        "the digest is not in the Decision"
    );
    assert!(
        serialized.contains(DIGEST_ALGORITHM),
        "the algorithm is not in the Decision"
    );
    assert!(
        serialized.contains(&digest.key_id),
        "the key id is not in the Decision"
    );

    // Neither the unkeyed hash of the body, nor any part of the body itself.
    let material = gate.digest_material(&candidate);
    let unkeyed = ring::digest::digest(&ring::digest::SHA256, &material);
    let unkeyed_hex: String = unkeyed
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_ne!(unkeyed_hex, digest.digest, "the digest is the unkeyed hash");
    assert!(
        !serialized.contains(&unkeyed_hex),
        "the Decision carries the unkeyed SHA-256 of the body"
    );
    let secret = fixture
        .secret
        .clone()
        .expect("the fixture names its secret");
    for window in secret.as_bytes().windows(8) {
        let fragment = std::str::from_utf8(window).expect("ascii fixtures");
        assert!(
            !serialized.contains(fragment),
            "the Decision carries {fragment:?} of the secret"
        );
    }

    // Recomputing with the stored key reproduces it.
    let key = DecisionKeyRepo::get(
        &node.handle(),
        &candidate.parts.scope,
        &DecisionKeyId::parse(&digest.key_id),
    )
    .await
    .expect("the key row reads")
    .expect("the key row is there");
    assert_eq!(key.digest(&material), digest.digest);

    // And a different scope cannot read that key (spec 012 B-3).
    assert!(
        DecisionKeyRepo::get(
            &node.handle(),
            &common::scope("somebody-else"),
            &DecisionKeyId::parse(&digest.key_id),
        )
        .await
        .expect("the key row reads")
        .is_none(),
        "a key was readable from another scope"
    );

    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr007_two_refusals_mint_two_keys_and_a_destroyed_key_cannot_be_recovered() {
    let node = common::node().await;
    let fixture = fixture("secret-github-token");
    let gate = fixture.gate();
    let operator = common::sub("auditor");

    // The same candidate twice. The bodies are identical and the material is
    // identical; the digests must not be.
    let first_candidate = fixture.candidate();
    let mut second_candidate = fixture.candidate();
    second_candidate.parts.scope = first_candidate.parts.scope.clone();
    assert_eq!(
        gate.digest_material(&first_candidate),
        gate.digest_material(&second_candidate),
        "the two candidates are not the same content"
    );

    let first = capture(&node, &gate, &first_candidate, &operator, None)
        .await
        .expect("the first refusal");
    let second = capture(&node, &gate, &second_candidate, &operator, None)
        .await
        .expect("the second refusal");

    let (a, b) = (
        first.digest.expect("a digest"),
        second.digest.expect("a digest"),
    );
    assert_ne!(a.key_id, b.key_id, "two refusals shared a key");
    assert_ne!(a.digest, b.digest, "two refusals produced one digest");
    assert_eq!(rows(&node, "decision_key").await, 2);

    // Erasure destroys the key of a scope (014 B-9), and after that nothing
    // returns the digest's input: the key is the only thing that could
    // confirm a guess, and it is gone.
    let mut txn = TxnBuilder::new();
    DecisionKeyRepo::stage_destroy_for_scope(&mut txn, &first_candidate.parts.scope);
    node.handle()
        .txn(txn.into_statements())
        .await
        .expect("the destruction commits");
    assert_eq!(rows(&node, "decision_key").await, 0);

    for digest in [&a, &b] {
        assert!(
            DecisionKeyRepo::get(
                &node.handle(),
                &first_candidate.parts.scope,
                &DecisionKeyId::parse(&digest.key_id),
            )
            .await
            .expect("the key row reads")
            .is_none(),
            "a destroyed key came back"
        );
    }

    // The chain still holds both Decisions and still verifies: the digests are
    // opaque values now, not missing ones.
    assert!(node.ledger.verify().await.is_ok());
    let chain = records(&node).await;
    let serialized: String = chain
        .iter()
        .map(|record| record.to_canonical_json().expect("a record serializes"))
        .collect();
    assert!(serialized.contains(&a.digest) && serialized.contains(&b.digest));

    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr003_an_override_admits_exactly_one_candidate_and_emits_one_decision() {
    let node = common::node().await;
    let fixture = fixture("secret-high-entropy-token");
    let gate = fixture.gate();
    let operator = common::sub("operator");
    let candidate = fixture.candidate();
    let other = fixture.candidate();
    assert_ne!(candidate.parts.id, other.parts.id);

    let decision_ref = DecisionRef::new(format!("{KIND_OVERRIDE}-{}", candidate.parts.id))
        .expect("a legal decision id");
    let over = Override::new(
        candidate.parts.id,
        "secret_detected",
        "the webhook secret was rotated an hour ago and this note is the rotation record",
        operator.clone(),
        decision_ref.clone(),
    )
    .expect("a justified override");

    // Exactly one candidate: the same override does not admit the next one.
    assert!(
        gate.evaluate_overridden(&other, &over).is_err(),
        "one override admitted a second candidate"
    );
    // Nor a refusal for another reason.
    let unestablished = {
        let mut other = fixture.candidate();
        other.origin = aicortex_gate::Origin::Unestablished;
        other
    };
    assert!(
        gate.evaluate_overridden(&unestablished, &over).is_err(),
        "an override crossed to another candidate"
    );
    // And an override needs a justification.
    assert!(
        Override::new(
            candidate.parts.id,
            "secret_detected",
            "   ",
            operator.clone(),
            decision_ref,
        )
        .is_err(),
        "an override without a justification was accepted"
    );

    let before = records(&node).await.len();
    let outcome = capture(&node, &gate, &candidate, &operator, Some(&over))
        .await
        .expect("the override admits");
    let Verdict::Admit(admitted) = &outcome.verdict else {
        panic!("the override did not admit: {:?}", outcome.verdict);
    };

    // Admitted with `trust: Assertion` and the marker in its provenance (B-5).
    assert_eq!(
        admitted.memory().trust,
        aicortex_types::TrustClass::Assertion
    );
    let marker = admitted
        .memory()
        .provenance
        .admission
        .as_ref()
        .expect("the override is marked in the provenance");
    assert_eq!(marker.reason_code, "secret_detected");
    assert_eq!(&marker.by, &operator);

    // Exactly one candidate stored, and the marker survives the round trip.
    assert_eq!(rows(&node, "memory").await, 1);
    let stored = MemoryRepo::new()
        .get(&node.handle(), &candidate.parts.scope, admitted.memory().id)
        .await
        .expect("the record reads")
        .expect("the record is there");
    assert_eq!(stored.provenance.admission.as_ref(), Some(marker));
    assert_eq!(stored.status, Status::Active);

    // Exactly one Decision, naming the operator subject.
    let entry = ledger_entry(
        KIND_OVERRIDE,
        &Reason::SecretDetected {
            detector: aicortex_gate::DetectorId::new("high-entropy-token"),
            offset: 0,
        },
        &candidate.parts.scope,
        &operator,
        &DigestRef {
            digest: "0".repeat(64),
            algorithm: DIGEST_ALGORITHM.to_owned(),
            key_id: "0".repeat(32),
        },
        Some(admitted.memory().id),
    );
    capture::append(&node, &entry, candidate.parts.id)
        .await
        .expect("the override is ledgered");
    let after = records(&node).await;
    assert_eq!(
        after.len() - before,
        1,
        "an override appended {} decisions",
        after.len() - before
    );
    let decision = after
        .last()
        .expect("a record")
        .decision()
        .expect("a decision");
    assert_eq!(decision.kind.as_str(), KIND_OVERRIDE);
    assert_eq!(&decision.actor, &operator);

    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b7_b9_a_quarantine_stores_an_invisible_row_and_a_key_that_erasure_reaches() {
    let node = common::node().await;
    let fixture = fixture("quarantine-origin-asserted");
    let candidate = fixture.candidate();
    let operator = common::sub("importer");

    let outcome = capture(&node, &fixture.gate(), &candidate, &operator, None)
        .await
        .expect("the capture path runs");
    let Verdict::Quarantine(admitted, Reason::OriginUnestablished { .. }) = &outcome.verdict else {
        panic!(
            "an asserted origin was not quarantined: {:?}",
            outcome.verdict
        );
    };
    let id = admitted.memory().id;

    // The row exists and is out of sight.
    let stored = MemoryRepo::new()
        .get(&node.handle(), &candidate.parts.scope, id)
        .await
        .expect("the record reads")
        .expect("the record is there");
    assert_eq!(stored.status, Status::Quarantined);
    assert!(
        !stored.is_retrievable(),
        "a quarantined memory is retrievable"
    );
    let listing = MemoryRepo::new()
        .list(
            &node.handle(),
            &aicortex_store::CursorKey::new([3u8; 32]),
            &candidate.parts.scope,
            &aicortex_store::MemoryFilter::active(),
            10,
            None,
        )
        .await
        .expect("a listing reads");
    assert!(
        listing.memories.is_empty(),
        "the default listing showed a quarantined memory"
    );

    // Its Decision's key names the memory, which is how 014 B-7 finds it.
    let digest = outcome.digest.expect("a quarantine digests");
    let key_id = DecisionKeyId::parse(&digest.key_id);
    assert!(
        DecisionKeyRepo::get(&node.handle(), &candidate.parts.scope, &key_id)
            .await
            .expect("the key row reads")
            .is_some()
    );
    let mut txn = TxnBuilder::new();
    DecisionKeyRepo::stage_destroy_for_memory(&mut txn, &candidate.parts.scope, id);
    node.handle()
        .txn(txn.into_statements())
        .await
        .expect("the destruction commits");
    assert!(
        DecisionKeyRepo::get(&node.handle(), &candidate.parts.scope, &key_id)
            .await
            .expect("the key row reads")
            .is_none(),
        "erasing the memory left its Decision's key behind"
    );

    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b9_an_admission_is_not_ledgered_and_leaves_no_key() {
    let node = common::node().await;
    let fixture = fixture("admit-plain-note");
    let candidate = fixture.candidate();
    let before = records(&node).await.len();

    let outcome = capture(
        &node,
        &fixture.gate(),
        &candidate,
        &common::sub("client"),
        None,
    )
    .await
    .expect("the capture path runs");
    assert!(matches!(outcome.verdict, Verdict::Admit(_)));
    assert!(outcome.decision.is_none());

    assert_eq!(rows(&node, "memory").await, 1);
    assert_eq!(rows(&node, "outbox").await, 1);
    assert_eq!(
        rows(&node, "decision_key").await,
        0,
        "an admission minted a digest key"
    );
    assert_eq!(
        records(&node).await.len(),
        before,
        "an admission was ledgered; B-9 ledgers refusals and quarantines"
    );

    node.shutdown().await;
}
