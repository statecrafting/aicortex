//! Real single-voter proof that a host owns one atomic Rahi transaction.

#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used
)]

use std::collections::BTreeMap;
use std::net::{SocketAddr, TcpListener};
use std::path::Path;
use std::time::Duration;

use aicortex_claims::{
    AdmissionRef, AsOf, ClaimHistory, ClaimRecord, ProjectionPolicy, RegistrySnapshot, TxBound,
    TxStamp, ValidBound, ValidTime, project,
};
use aicortex_embed::{
    ChunkConfig, Chunker, EmbeddingProvider, EmbeddingWorker, ModelId, ModelRegistry,
    ModelRevision, Vector, WorkerConfig, embedding_work_key,
};
use aicortex_external_host_fixture::ExternalHost;
use aicortex_gate::{
    AdmissionPolicy, Candidate, ClaimContext, ClaimVerdict, Gate, Origin, SourceState, Verdict,
};
use aicortex_store::{
    ClaimAppend, ClaimProposalRepo, ClaimRepo, EmbeddingTarget, MemoryRepo,
    OperatorPredicateConfig, PredicateRegistrationGrant, PredicateRegistryRepo, Registration,
    ScopeId, document_digest,
};
use aicortex_types::{
    Actor, ActorId, AuthorityLevel, CivilDate, Claim, ClaimId, ClaimParts, ClaimProposal,
    ClaimValue, EpistemicStatus, Evidence, Importance, Memory, MemoryBody, MemoryId, MemoryKind,
    MemoryParts, Namespace, PredicateRef, PredicateSet, ProposalId, Provenance, Scope, SourceRef,
    SourceSystem, Stance, SubjectKey, SubjectKind, SubjectRef, SupersessionRule, TimePoint,
    TimeValue, TrustClass, ValidTimeMode, Zone, ZonedTime,
};
use rahi_cli::Cell;
use rahi_store::{
    ContentDigest, EncKey, EncKeys, Envelope, Outbox, ReceiptKey, ReceiptMeta, Receipts,
    RetryPolicy, Statement, Store, StoreConfig, StoreHandle, StoreSecrets, TxnBuilder, Value, Work,
};
use rahi_types::{Error, Revision, Sub, UnixSeconds};
use serde::Deserialize;

fn at(offset: u64) -> UnixSeconds {
    UnixSeconds::new(1_800_000_000 + offset)
}

fn scope() -> Scope {
    Scope::personal(Sub::new("host-traveler"))
}

fn registry_set() -> PredicateSet {
    serde_json::from_str(include_str!(
        "../../../crates/aicortex-claims/testdata/registries/travel-v1.json"
    ))
    .expect("the registry fixture parses")
}

fn observation() -> Memory {
    Memory::new(MemoryParts {
        id: MemoryId::now_v7(),
        scope: scope(),
        kind: MemoryKind::Observation,
        body: MemoryBody::text("Supplier confirmed booking locator HOST1"),
        actor: Actor::human(ActorId::new("host-traveler").expect("an actor")),
        provenance: Provenance::captured(
            SourceRef::new(SourceSystem::new("external-host").expect("a source system")),
            at(1),
            at(2),
        ),
        trust: TrustClass::Assertion,
        importance: Importance::at(at(2)).expect("default importance"),
        created: at(2),
    })
}

fn admitted_observation(memory: &Memory) -> aicortex_gate::Admitted {
    let parts = MemoryParts {
        id: memory.id,
        scope: memory.scope.clone(),
        kind: memory.kind,
        body: memory.body.clone(),
        actor: memory.actor.clone(),
        provenance: memory.provenance.clone(),
        trust: memory.trust.clone(),
        importance: memory.importance,
        created: memory.created,
    };
    match Gate::standard().evaluate(&Candidate::new(parts, Origin::Authenticated)) {
        Verdict::Admit(admitted) => admitted,
        other => panic!("fixture observation was not admitted: {other:?}"),
    }
}

fn proposal(source: MemoryId) -> ClaimProposal {
    let claim = Claim::new(ClaimParts {
        id: ClaimId::now_v7(),
        scope: scope(),
        subject: SubjectRef {
            namespace: Namespace::new("travel").expect("a namespace"),
            kind: SubjectKind::new("booking").expect("a subject kind"),
            key: SubjectKey::new("host-booking-1").expect("a subject key"),
        },
        predicate: PredicateRef::parse("travel:booking.locator@1").expect("a predicate"),
        value: ClaimValue::Text("HOST1".to_owned()),
        slot: None,
        epistemic: EpistemicStatus::plain(Stance::Asserted).expect("an epistemic status"),
        provenance: Provenance::captured(
            SourceRef::new(SourceSystem::new("external-host").expect("a source system")),
            at(1),
            at(2),
        )
        .derived(
            vec![source],
            aicortex_types::ExtractorVersion {
                name: "host-extractor".to_owned(),
                version: "1".to_owned(),
            },
        ),
    });
    ClaimProposal {
        id: ProposalId::now_v7(),
        claim,
        proposer: Actor::human(ActorId::new("host-traveler").expect("an actor")),
        evidence: vec![Evidence::UserStatement {
            by: Sub::new("host-traveler"),
            authority: AuthorityLevel::UserAsserted,
        }],
        relations: Vec::new(),
        proposed_at: at(3),
    }
}

fn admitted_claim(proposal: &ClaimProposal, source: MemoryId) -> aicortex_gate::AdmittedClaim {
    let registry = RegistrySnapshot::from_sets([registry_set()]).expect("a legal registry");
    let context = ClaimContext {
        sources: BTreeMap::from([(source, SourceState::Available)]),
        targets: BTreeMap::new(),
    };
    match Gate::standard().evaluate_claim(
        proposal,
        &registry,
        &AdmissionPolicy::initial(),
        &context,
    ) {
        ClaimVerdict::Admit(admitted) => *admitted,
        other => panic!("fixture claim was not admitted: {other:?}"),
    }
}

fn projection_document(admitted: &aicortex_gate::AdmittedClaim) -> serde_json::Value {
    let mut history = ClaimHistory::new();
    history
        .append_claim(ClaimRecord {
            claim: admitted.claim().clone(),
            valid: ValidTime::Timeless,
            source_time: None,
            source_seq: None,
            tx: TxStamp {
                seq: 1,
                recorded_at: at(4),
            },
            authority: admitted.authority(),
            admission: AdmissionRef {
                proposal: admitted.proposal(),
                policy_id: admitted.policy().id.clone(),
                policy_version: admitted.policy().version,
                sourcing: admitted.sourcing(),
            },
            supersession: SupersessionRule::ExplicitOnly,
            hostile_content: false,
            origin_erased: false,
        })
        .expect("the admitted claim appends to history");
    let point = TimePoint::exact(TimeValue::DateTime(
        ZonedTime::minute(
            CivilDate::ymd(2026, 10, 3).expect("a date"),
            12,
            0,
            Zone::offset(0).expect("UTC offset"),
        )
        .expect("a time"),
    ));
    let view = project(
        &history,
        AsOf {
            knowledge: TxBound::Sequence(1),
            valid: ValidBound::Instant(point),
        },
        &ProjectionPolicy::v1(),
    )
    .expect("the host projection is pure");
    serde_json::to_value(view).expect("the projection serializes")
}

fn stage_atomic(txn: &mut TxnBuilder, embedding: &EmbeddingTarget, fail_last: bool) {
    let receipt = ReceiptKey::new("host", "mail:fixture", "message-1").expect("a receipt key");
    Receipts::stage_first(
        txn,
        &receipt,
        &ContentDigest::of(b"fixture message"),
        &ReceiptMeta::default(),
        at(1),
    )
    .expect("the receipt stages");

    let memory = observation();
    let admitted_memory = admitted_observation(&memory);
    let memory_work = Envelope::new(
        "memory",
        Some(scope().owner.as_str().to_owned()),
        memory.id.to_string(),
        Revision::new(1),
    );
    MemoryRepo::new()
        .insert(
            txn,
            &admitted_memory,
            &memory.provenance,
            embedding,
            &memory_work,
        )
        .expect("the source observation stages");

    let proposal = proposal(memory.id);
    ClaimProposalRepo::stage_append(txn, &proposal).expect("the proposal stages");
    let admitted = admitted_claim(&proposal, memory.id);
    let append = ClaimAppend {
        admitted: &admitted,
        valid: ValidTime::Timeless,
        source_time: None,
        source_seq: None,
        valid_time_mode: ValidTimeMode::Timeless,
        supersession: SupersessionRule::ExplicitOnly,
        hostile_content: false,
    };
    let claim_work = Envelope::new(
        "claim",
        Some(scope().owner.as_str().to_owned()),
        proposal.claim.id.to_string(),
        Revision::new(1),
    );
    ClaimRepo::stage_append(
        txn,
        &ClaimHistory::new(),
        &[append],
        &Sub::new("host-writer"),
        at(4),
        &claim_work,
    )
    .expect("the admitted claim stages");

    let projection = projection_document(&admitted);
    txn.push(Statement::with_params(
        "INSERT INTO host_projection_revision
            (revision_id, subject_key, projection_digest, document)
         VALUES (?1, ?2, ?3, ?4)",
        vec![
            Value::from("host-projection-1"),
            Value::from(proposal.claim.subject.key.as_str()),
            Value::from(
                projection["digest"]
                    .as_str()
                    .expect("the projection has a digest"),
            ),
            Value::from(serde_json::to_string(&projection).expect("projection JSON")),
        ],
    ));
    Outbox::stage(
        txn,
        &Envelope::new(
            "host-projection",
            Some("host".to_owned()),
            "host-projection-1",
            Revision::new(1),
        ),
    );
    if fail_last {
        txn.push(Statement::new(
            "INSERT INTO deliberately_missing_last_statement VALUES (1)",
        ));
    }
}

/// The embedding model the host configures and activates.
fn host_model() -> ModelRevision {
    ModelRevision {
        model_id: ModelId::new("external-host-test").expect("a model id"),
        revision: 1,
        dims: 3,
        normalized: true,
        first_seen: at(0),
        active: true,
    }
}

/// A deterministic provider standing in for the host's configured model.
#[derive(Clone, Copy, Debug)]
struct HostProvider;

impl EmbeddingProvider for HostProvider {
    fn id(&self) -> ModelId {
        host_model().model_id
    }

    fn dims(&self) -> u16 {
        host_model().dims
    }

    fn normalized(&self) -> bool {
        host_model().normalized
    }

    async fn embed(&self, batch: &[&str]) -> Result<Vec<Vector>, Error> {
        batch
            .iter()
            .map(|_| Vector::new(vec![1.0, 0.0, 0.0]))
            .collect()
    }
}

fn host_worker() -> EmbeddingWorker<HostProvider> {
    EmbeddingWorker::new(
        HostProvider,
        host_model(),
        Chunker::new(ChunkConfig {
            threshold_bytes: 256,
            target_bytes: 128,
            overlap_bytes: 16,
            max_chunks_per_memory: 512,
        })
        .expect("chunk config"),
        WorkerConfig {
            batch_size: 16,
            hold_for: Duration::from_secs(5),
            retry: RetryPolicy {
                max_attempts: 3,
                base: Duration::from_secs(1),
                cap: Duration::from_secs(2),
            },
        },
        "external-host-worker",
    )
    .expect("the worker matches the active model")
}

fn free_addr() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a free port");
    listener.local_addr().expect("the allocated address")
}

fn store_config(data_dir: &Path) -> StoreConfig {
    StoreConfig {
        node_id: 1,
        nodes: Vec::new(),
        data_dir: data_dir.to_path_buf(),
        raft_addr: free_addr(),
        api_addr: free_addr(),
        secrets: StoreSecrets {
            secret_raft: "external-host-raft-secret".to_owned(),
            secret_api: "external-host-api-secret".to_owned(),
            enc_keys: EncKeys {
                active: "test".to_owned(),
                keys: vec![EncKey {
                    id: "test".to_owned(),
                    key: vec![53u8; 32],
                }],
            },
        },
        backup_keep_days: 1,
        s3: None,
    }
}

#[derive(Deserialize)]
struct Count {
    count: i64,
}

async fn count(store: &StoreHandle, sql: &str) -> i64 {
    let rows: Vec<Count> = store
        .query_consistent(sql.to_owned(), Vec::new())
        .await
        .expect("the count reads");
    rows[0].count
}

async fn assert_atomic_counts(store: &StoreHandle, expected: i64) {
    for (label, sql) in [
        ("receipt", "SELECT count(*) AS count FROM rahi_receipt"),
        ("memory", "SELECT count(*) AS count FROM memory"),
        (
            "embedding work",
            "SELECT count(*) AS count FROM rahi_processing WHERE namespace = 'aicortex.memory'",
        ),
        ("proposal", "SELECT count(*) AS count FROM claim_proposal"),
        ("claim", "SELECT count(*) AS count FROM claim_history"),
        ("admission", "SELECT count(*) AS count FROM claim_admission"),
        (
            "projection",
            "SELECT count(*) AS count FROM host_projection_revision",
        ),
        (
            "host outbox",
            "SELECT count(*) AS count FROM outbox WHERE kind = 'host-projection'",
        ),
    ] {
        assert_eq!(count(store, sql).await, expected, "{label} count");
    }
    // Spec 015 B-1: the host's memory insert staged its embedding job for the
    // active revision in the same transaction, or nothing at all.
    let embed = Work::counts(store)
        .await
        .expect("the work queue reads")
        .into_iter()
        .find(|queue| queue.processor == "embed.r1")
        .map_or(0, |queue| queue.pending);
    assert_eq!(
        embed,
        u64::try_from(expected).unwrap(),
        "embedding job count"
    );
}

async fn migrate(store: &StoreHandle) {
    let app = <ExternalHost as Cell>::migrations();
    let sets = <ExternalHost as Cell>::migration_sets();
    let expected = app.len() + sets.iter().map(|set| set.migrations.len()).sum::<usize>();
    let report = store
        .migrate_sets(app, &sets)
        .await
        .expect("all host and library migrations apply");
    assert_eq!(report.applied.len(), expected);

    let mut txn = TxnBuilder::new();
    ModelRegistry::activate(&mut txn, &host_model()).expect("active model accepted");
    store
        .txn(txn.into_statements())
        .await
        .expect("the host activates its embedding model");
}

async fn register_once(store: &StoreHandle) {
    let set = registry_set();
    let config = OperatorPredicateConfig::new(
        Sub::new("host-operator"),
        vec![PredicateRegistrationGrant::new(
            set.namespace().clone(),
            set.version(),
            document_digest(&set).expect("the document digests"),
        )],
    )
    .expect("one grant");
    let snapshot = PredicateRegistryRepo::snapshot(store)
        .await
        .expect("the empty registry reads");
    let plan = PredicateRegistryRepo::validate_registration(&snapshot, &set, &config)
        .expect("the operator grant validates");
    let mut txn = TxnBuilder::new();
    assert!(matches!(
        PredicateRegistryRepo::stage_registration(&mut txn, plan, at(1)),
        Ok(Registration::Registered(_))
    ));
    store
        .txn(txn.into_statements())
        .await
        .expect("the registry row commits");

    let snapshot = PredicateRegistryRepo::snapshot(store)
        .await
        .expect("the registry reads");
    let plan = PredicateRegistryRepo::validate_registration(&snapshot, &set, &config)
        .expect("identical registration validates as a no-op");
    let mut txn = TxnBuilder::new();
    assert_eq!(
        PredicateRegistryRepo::stage_registration(&mut txn, plan, at(2)).expect("the no-op stages"),
        Registration::Unchanged
    );
    assert!(txn.is_empty());
    assert_eq!(
        count(store, "SELECT count(*) AS count FROM predicate_registry").await,
        1
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr006_fr008_commit_restart_noop_and_checksum_refusal() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let config = store_config(&dir.path().join("store"));
    let store = Store::open(&config).await.expect("the host store opens");
    let handle = store.handle();
    migrate(&handle).await;
    register_once(&handle).await;

    let mut txn = TxnBuilder::new();
    let embedding = EmbeddingTarget::observe(&handle)
        .await
        .expect("the embedding registry reads");
    stage_atomic(&mut txn, &embedding, false);
    handle
        .txn(txn.into_statements())
        .await
        .expect("the host-owned transaction commits");
    assert_atomic_counts(&handle, 1).await;

    let again = handle
        .migrate_sets(
            <ExternalHost as Cell>::migrations(),
            &<ExternalHost as Cell>::migration_sets(),
        )
        .await
        .expect("an identical apply is a no-op");
    assert!(again.applied.is_empty());
    assert_eq!(
        count(
            &handle,
            "SELECT count(*) AS count FROM schema_set_version WHERE set_name = 'aicortex'",
        )
        .await,
        i64::from(aicortex_store::EXPECTED_SCHEMA_VERSION)
    );

    let mut altered = aicortex_store::migration_set().expect("the set builds");
    altered.migrations[0].sql.push_str(" SELECT 1");
    assert!(
        handle
            .migrate_sets(
                <ExternalHost as Cell>::migrations(),
                &[
                    rahi_store::coordination_set(),
                    rahi_store::receipt_set(),
                    altered
                ],
            )
            .await
            .is_err(),
        "an edited migration must be refused"
    );
    assert_atomic_counts(&handle, 1).await;

    store.shutdown().await.expect("the host store stops");
    let reopened = Store::open(&config).await.expect("the host store restarts");
    assert_atomic_counts(&reopened.handle(), 1).await;
    assert_eq!(
        count(
            &reopened.handle(),
            "SELECT count(*) AS count FROM predicate_registry",
        )
        .await,
        1
    );
    reopened.shutdown().await.expect("the reopened host stops");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr007_last_statement_failure_rolls_every_system_back() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let config = store_config(&dir.path().join("rollback-store"));
    let store = Store::open(&config).await.expect("the host store opens");
    let handle = store.handle();
    migrate(&handle).await;

    let mut txn = TxnBuilder::new();
    let embedding = EmbeddingTarget::observe(&handle)
        .await
        .expect("the embedding registry reads");
    stage_atomic(&mut txn, &embedding, true);
    assert!(
        handle.txn(txn.into_statements()).await.is_err(),
        "the injected last statement must fail"
    );
    assert_atomic_counts(&handle, 0).await;
    store.shutdown().await.expect("the host store stops");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn host_capture_stages_embedding_work_the_worker_drains() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let config = store_config(&dir.path().join("embedding-store"));
    let store = Store::open(&config).await.expect("the host store opens");
    let handle = store.handle();
    migrate(&handle).await;

    let memory = observation();
    let admitted = admitted_observation(&memory);
    let work = Envelope::new(
        "memory",
        Some(scope().owner.as_str().to_owned()),
        memory.id.to_string(),
        Revision::new(1),
    );
    let embedding = EmbeddingTarget::observe(&handle)
        .await
        .expect("the host observes its active model through the leader");
    let mut txn = TxnBuilder::new();
    MemoryRepo::new()
        .insert(&mut txn, &admitted, &memory.provenance, &embedding, &work)
        .expect("the host capture stages");
    handle
        .txn(txn.into_statements())
        .await
        .expect("the host-owned capture commits");

    // The durable identity the worker path stages through `stage_embedding`.
    let scope_id = ScopeId::of(&memory.scope);
    let key =
        embedding_work_key(scope_id.as_str(), memory.id, &host_model()).expect("a processing key");
    let staged: Vec<Count> = handle
        .query_consistent(
            "SELECT count(*) AS count FROM rahi_processing
             WHERE key_digest = ?1 AND revision = ?2 AND processor = ?3
               AND processor_revision = ?4 AND state = 'pending'"
                .to_owned(),
            vec![
                Value::from(key.receipt.key_digest()),
                Value::Integer(i64::from(key.revision)),
                Value::from(key.processor.as_str()),
                Value::from(key.processor_revision.as_str()),
            ],
        )
        .await
        .expect("the queue reads");
    assert_eq!(
        staged[0].count, 1,
        "a host capture must stage its embedding work in the same transaction"
    );

    let report = host_worker()
        .drain(&handle, at(10))
        .await
        .expect("the worker drains");
    assert_eq!(report.completed, 1);
    let embedded: Vec<Count> = handle
        .query_consistent(
            "SELECT count(*) AS count FROM embedding
             WHERE scope_id = ?1 AND memory_id = ?2 AND model_revision = 1"
                .to_owned(),
            vec![
                Value::from(scope_id.as_str()),
                Value::from(memory.id.to_string()),
            ],
        )
        .await
        .expect("the embeddings read");
    assert_eq!(embedded[0].count, 1, "the host capture is embedded");
    store.shutdown().await.expect("the host store stops");
}
