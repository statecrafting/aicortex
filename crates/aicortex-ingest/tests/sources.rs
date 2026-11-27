//! The import adapters, against a real node (spec 031 B-1, B-2, B-4, B-9 to
//! B-11 and FR-001 to FR-008, FR-012).
//!
//! Every test uploads a committed synthetic export to a staging area, drives
//! the shipped runner over the real gate and store, and reads the store back.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

mod common;

use std::path::PathBuf;
use std::sync::Arc;

use aicortex_ingest::sources::chat_export::SOURCE;
use aicortex_ingest::upload::{Staging, UploadId};
use aicortex_ingest::{IngestPolicy, Registry, RunCounts, RunReport, RunState, Runner};
use aicortex_store::provenance_repo::{ProvenanceRepo, TurnRange};
use aicortex_store::{Authority, Eraser, Erasure};
use aicortex_types::{ActorKind, ConversationId, Memory, Scope, SourceSystem, TrustClass};
use common::{Node, ingest_service, manifest, node, request, scope};
use rahi_types::{Sub, UnixSeconds};
use serde_json::json;

fn fixture(name: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("testdata/exports")
        .join(name);
    std::fs::read(path).expect("a committed fixture")
}

struct Rig {
    node: Node,
    staging: Arc<Staging>,
    runner: Runner,
}

async fn rig() -> Rig {
    let node = node().await;
    let staging = Arc::new(Staging::new(node.dir.path().join("staging")));
    let mut registry = Registry::new();
    registry
        .register_imports(&staging, &manifest(&[]), &ingest_service())
        .expect("the imports register");
    let runner = Runner::new(Arc::new(registry), None, IngestPolicy::default());
    Rig {
        node,
        staging,
        runner,
    }
}

impl Rig {
    fn upload(&self, scope: &Scope, file: &str) -> UploadId {
        self.staging.put(scope, &fixture(file)).expect("it stages")
    }

    async fn run(&self, scope: &Scope, upload: &UploadId, dry: bool) -> RunReport {
        let mut request = request(scope, SOURCE);
        request.options = json!({ "upload": upload.as_str() });
        request.dry_run = dry;
        self.runner
            .run(&self.node.runtime(), request)
            .await
            .expect("the run starts")
    }

    async fn import(&self, scope: &Scope, file: &str) -> RunReport {
        let upload = self.upload(scope, file);
        self.run(scope, &upload, false).await
    }

    async fn turns(&self, scope: &Scope, id: &str) -> Vec<Memory> {
        ProvenanceRepo::conversation_turns(
            &self.node.handle(),
            scope,
            &SourceSystem::new(SOURCE).unwrap(),
            &ConversationId::new(id).unwrap(),
            TurnRange::All,
        )
        .await
        .expect("the turns read")
    }
}

fn turn_numbers(memories: &[Memory]) -> Vec<u32> {
    memories
        .iter()
        .map(|memory| memory.provenance.conversation.as_ref().unwrap().turn)
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr006_a_conversation_is_one_memory_per_stored_message_with_gaps_kept() {
    let rig = rig().await;
    let alice = scope("alice");
    let report = rig.import(&alice, "flat.json").await;
    assert_eq!(report.state, RunState::Completed, "{report:?}");
    assert_eq!(
        report.counts,
        RunCounts {
            seen: 5,
            admitted: 3,
            refused: 1,
            unmapped: 1,
            ..RunCounts::default()
        },
        "two turns the gate refused or the adapter skipped are counted, not stored"
    );

    let turns = rig.turns(&alice, "flat-c1").await;
    assert_eq!(turn_numbers(&turns), vec![1, 2, 5], "the gaps stay gaps");
    assert_eq!(rig.node.memory_rows(&alice).await, 3);
    for memory in &turns {
        let turn = memory.provenance.conversation.as_ref().unwrap().turn;
        let assistant = turn == 2;
        assert_eq!(memory.trust, TrustClass::Assertion, "turn {turn}");
        assert_eq!(
            matches!(memory.actor.kind, ActorKind::Agent { .. }),
            assistant,
            "turn {turn}: only an assistant turn is an agent's"
        );
        assert!(!memory.provenance.is_derived());
    }
    assert_eq!(turns[0].body.text, "Alice owns the billing project.");
    assert_eq!(turns[0].actor.label.as_deref(), Some("user"));
    rig.node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr002_a_reimport_creates_no_new_memory_and_the_upload_is_gone_after_a_run() {
    let rig = rig().await;
    let alice = scope("alice");
    let upload = rig.upload(&alice, "flat.json");
    let first = rig.run(&alice, &upload, false).await;
    assert_eq!(first.counts.admitted, 3, "{first:?}");
    assert!(
        rig.staging.read(&alice, &upload).is_err(),
        "a completed run deletes what it read (B-1)"
    );

    let second = rig.import(&alice, "flat.json").await;
    assert_eq!(second.counts.admitted, 0, "{second:?}");
    assert_eq!(second.counts.updated, 3, "{second:?}");
    assert_eq!(rig.node.memory_rows(&alice).await, 3);
    assert_eq!(rig.node.item_rows(&alice).await, 3);
    rig.node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dry_run_counts_what_the_real_run_counts_and_keeps_the_upload() {
    let rig = rig().await;
    let alice = scope("alice");
    let upload = rig.upload(&alice, "flat.json");
    let dry = rig.run(&alice, &upload, true).await;
    assert_eq!(
        rig.node.memory_rows(&alice).await,
        0,
        "a dry run writes nothing"
    );
    assert!(
        rig.staging.read(&alice, &upload).is_ok(),
        "a dry run leaves the file for the real run"
    );
    let real = rig.run(&alice, &upload, false).await;
    assert_eq!(dry.counts, real.counts);
    rig.node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr001_the_tree_shape_is_detected_by_content_and_keeps_the_branch_it_follows() {
    let rig = rig().await;
    let alice = scope("alice");
    let report = rig.import(&alice, "tree.json").await;
    assert_eq!(report.state, RunState::Completed, "{report:?}");
    assert_eq!(
        report.counts,
        RunCounts {
            seen: 5,
            admitted: 4,
            unmapped: 1,
            ..RunCounts::default()
        },
        "the system node has no text, and the abandoned branch is not read"
    );
    let turns = rig.turns(&alice, "tree-c1").await;
    assert_eq!(turn_numbers(&turns), vec![2, 3, 4, 5]);
    let authors: Vec<_> = turns
        .iter()
        .map(|m| m.actor.kind.label().to_owned())
        .collect();
    assert_eq!(authors, ["import", "agent", "import", "agent"]);
    assert!(
        turns
            .iter()
            .all(|m| !m.body.text.contains("abandoned branch"))
    );
    rig.node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr005_an_injection_in_a_transcript_is_stored_as_content_and_never_as_instruction() {
    let rig = rig().await;
    let alice = scope("alice");
    rig.import(&alice, "tree.json").await;
    let turns = rig.turns(&alice, "tree-c1").await;
    let hostile = turns
        .iter()
        .find(|m| m.body.text.contains("IGNORE PREVIOUS INSTRUCTIONS"))
        .expect("stored verbatim as the user's words");
    assert_eq!(hostile.trust, TrustClass::Assertion);
    assert_eq!(hostile.provenance.conversation.as_ref().unwrap().turn, 4);
    rig.node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr007_erasing_one_turn_leaves_the_others_and_the_read_skips_it() {
    let rig = rig().await;
    let alice = scope("alice");
    rig.import(&alice, "tree.json").await;
    let turns = rig.turns(&alice, "tree-c1").await;
    let bodies: Vec<_> = turns.iter().map(|m| m.body.text.clone()).collect();
    let victim = turns[1].id;
    Eraser::new()
        .erase(
            &rig.node.handle(),
            &rig.node.ledger,
            &Erasure::new(
                &alice,
                victim,
                &Authority::of(Sub::new("alice")),
                UnixSeconds::new(1_800_000_000),
            ),
        )
        .await
        .expect("the erasure runs");
    let remaining = rig.turns(&alice, "tree-c1").await;
    assert_eq!(turn_numbers(&remaining), vec![2, 4, 5]);
    let kept: Vec<_> = remaining.iter().map(|m| m.body.text.clone()).collect();
    assert_eq!(
        kept,
        [bodies[0].clone(), bodies[2].clone(), bodies[3].clone()]
    );
    rig.node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr008_the_same_id_in_another_scope_is_another_conversation() {
    let rig = rig().await;
    let (alice, bob) = (scope("alice"), scope("bob"));
    rig.import(&alice, "flat.json").await;
    rig.import(&bob, "flat.json").await;
    let a = rig.turns(&alice, "flat-c1").await;
    let b = rig.turns(&bob, "flat-c1").await;
    assert_eq!((a.len(), b.len()), (3, 3));
    assert!(a.iter().all(|m| m.scope == alice));
    assert!(b.iter().all(|m| m.scope == bob));
    rig.node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr012_ids_are_derived_when_the_export_has_none_and_a_duplicate_is_refused() {
    let expected: serde_json::Value =
        serde_json::from_slice(&fixture("flat-noid.expected.json")).unwrap();
    let ids: Vec<String> = expected["ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|id| id.as_str().unwrap().to_owned())
        .collect();
    let rig = rig().await;
    let alice = scope("alice");
    let first = rig.import(&alice, "flat-noid.json").await;
    assert_eq!(first.state, RunState::Completed, "{first:?}");
    assert_eq!(first.counts.unmapped, expected["refused"].as_u64().unwrap());
    assert_eq!(first.counts.admitted, 3, "{first:?}");
    assert_eq!(rig.turns(&alice, &ids[0]).await.len(), 2);
    assert_eq!(rig.turns(&alice, &ids[1]).await.len(), 1);

    let second = rig.import(&alice, "flat-noid.json").await;
    assert_eq!(second.counts.admitted, 0, "the same ids on a re-import");
    assert_eq!(second.counts.updated, 3, "{second:?}");
    assert_eq!(rig.node.memory_rows(&alice).await, 3);
    rig.node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b1_an_upload_belongs_to_its_scope_and_expires() {
    let node = node().await;
    let staging = Staging::new(node.dir.path().join("staging"));
    let (alice, bob) = (scope("alice"), scope("bob"));
    let id = staging.put(&alice, b"[]").unwrap();
    assert!(
        staging.read(&bob, &id).is_err(),
        "another scope's id names nothing"
    );
    assert_eq!(staging.read(&alice, &id).unwrap(), b"[]");
    assert!(UploadId::parse("../escape").is_err());
    assert!(staging.put(&alice, b"").is_err());

    let short = Staging::new(node.dir.path().join("staging")).with_ttl(std::time::Duration::ZERO);
    std::thread::sleep(std::time::Duration::from_millis(20));
    assert!(short.read(&alice, &id).is_err(), "an expired file is gone");
    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr004_the_import_adapters_declare_no_host() {
    let rig = rig().await;
    for source in rig.runner.registry().describe() {
        assert!(source.hosts.is_empty(), "{}", source.id);
    }
    assert_eq!(rig.runner.registry().len(), 1);
    rig.node.shutdown().await;
}
