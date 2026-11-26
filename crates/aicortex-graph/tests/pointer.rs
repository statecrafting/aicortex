//! Entity pointers (spec 026 B-1 to B-4, B-6, B-7; FR-001 to FR-003, FR-005,
//! FR-009).
//!
//! The graph is written as rows with fixed ids and times, so the committed
//! pointers in `testdata/pointers/` are the same bytes on every run. Rows are
//! written the way the extraction worker writes them; what the resolver does
//! with them is under test, not how they got there.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

mod common;

use std::fs;
use std::path::Path;
use std::sync::Mutex;

use aicortex_graph::{DEFAULT_TEXT_BYTES, PointerBounds, PointerSet, normalize, pointers};
use aicortex_store::{ReadStore, ScopeId};
use aicortex_types::{
    Actor, ActorId, Importance, Memory, MemoryBody, MemoryId, MemoryKind, MemoryParts, Provenance,
    Scope, SourceRef, SourceSystem, TrustClass,
};
use rahi_store::{StoreHandle, Value};
use rahi_types::{Error, UnixSeconds};
use serde::de::DeserializeOwned;
use serde_json::{Value as Json, json};

#[derive(serde::Deserialize)]
struct Count {
    n: i64,
}

fn memory_id(tail: u32) -> MemoryId {
    MemoryId::parse(&format!("019c4f00-0000-7000-8000-{tail:012}")).unwrap()
}

/// A store handle that records every statement it is asked to read, with the
/// values it was bound to.
struct Recording {
    inner: StoreHandle,
    seen: Mutex<Vec<(String, Vec<Value>)>>,
}

impl ReadStore for Recording {
    fn query<T: DeserializeOwned + Send + 'static>(
        &self,
        sql: &'static str,
        values: Vec<Value>,
    ) -> impl std::future::Future<Output = Result<Vec<T>, Error>> + Send {
        self.seen
            .lock()
            .unwrap()
            .push((sql.to_owned(), values.clone()));
        ReadStore::query(&self.inner, sql, values)
    }

    fn query_consistent<T: DeserializeOwned + Send + 'static>(
        &self,
        sql: &'static str,
        values: Vec<Value>,
    ) -> impl std::future::Future<Output = Result<Vec<T>, Error>> + Send {
        self.seen
            .lock()
            .unwrap()
            .push((sql.to_owned(), values.clone()));
        ReadStore::query_consistent(&self.inner, sql, values)
    }
}

/// One scope's rows, written directly.
struct Fixture {
    store: StoreHandle,
    scope: Scope,
    scope_id: String,
}

impl Fixture {
    async fn new(store: StoreHandle, owner: &str) -> Self {
        let (scope, scope_id) = common::scope(owner);
        store
            .execute(
                "INSERT INTO scope (scope_id, owner, kind, key, created)
                 VALUES (?1, ?2, ?3, ?4, 1700000000) ON CONFLICT DO NOTHING",
                vec![
                    Value::from(scope_id.as_str()),
                    Value::from(scope.owner.as_str()),
                    Value::from(scope.kind.label()),
                    Value::from(ScopeId::key_of(&scope)),
                ],
            )
            .await
            .unwrap();
        Self {
            store,
            scope,
            scope_id,
        }
    }

    async fn memory(&self, tail: u32, created: u64, trust: TrustClass, status: &str) -> MemoryId {
        let time = UnixSeconds::new(created);
        let memory = Memory::new(MemoryParts {
            id: memory_id(tail),
            scope: self.scope.clone(),
            kind: MemoryKind::Observation,
            body: MemoryBody::text("a memory the pointers never read"),
            actor: Actor::human(ActorId::new("tester").unwrap()),
            provenance: Provenance::captured(
                SourceRef::new(SourceSystem::new("test").unwrap()),
                time,
                time,
            ),
            trust,
            importance: Importance::at(time).unwrap(),
            created: time,
        });
        self.store
            .execute(
                "INSERT INTO memory
                 (id, scope_id, status, kind, trust, fingerprint, schema_version, body_bytes,
                  created, updated, record)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?1, 1, 1, ?6, ?6, ?7)",
                vec![
                    Value::from(memory.id.to_string()),
                    Value::from(self.scope_id.as_str()),
                    Value::from(status),
                    Value::from(memory.kind.label()),
                    Value::from(memory.trust.label()),
                    Value::Integer(i64::try_from(created).unwrap()),
                    Value::from(serde_json::to_string(&memory).unwrap()),
                ],
            )
            .await
            .unwrap();
        memory.id
    }

    async fn entity(&self, id: &str, kind: &str, canonical: &str, merged_into: Option<&str>) {
        self.store
            .execute(
                "INSERT INTO entity
                 (entity_id, scope_id, kind, canonical, first_seen, last_seen, merged_into,
                  merged_at)
                 VALUES (?1, ?2, ?3, ?4, 1700000000, 1700000200, ?5, ?6)",
                vec![
                    Value::from(id),
                    Value::from(self.scope_id.as_str()),
                    Value::from(kind),
                    Value::from(canonical),
                    Value::from(merged_into.map(str::to_owned)),
                    Value::from(merged_into.map(|_| 1_700_000_300_i64)),
                ],
            )
            .await
            .unwrap();
        self.alias(id, canonical).await;
    }

    async fn alias(&self, entity: &str, alias: &str) {
        self.store
            .execute(
                "INSERT INTO entity_alias (scope_id, entity_id, alias, normalized)
                 VALUES (?1, ?2, ?3, ?4)",
                vec![
                    Value::from(self.scope_id.as_str()),
                    Value::from(entity),
                    Value::from(alias),
                    Value::from(normalize(alias)),
                ],
            )
            .await
            .unwrap();
    }

    async fn mention(&self, memory: MemoryId, entity: &str, start: i64, end: i64) {
        self.store
            .execute(
                "INSERT INTO mention
                 (scope_id, memory_id, entity_id, byte_start, byte_end, confidence, extractor,
                  extractor_version)
                 VALUES (?1, ?2, ?3, ?4, ?5, 900, 'pattern', 1)",
                vec![
                    Value::from(self.scope_id.as_str()),
                    Value::from(memory.to_string()),
                    Value::from(entity),
                    Value::Integer(start),
                    Value::Integer(end),
                ],
            )
            .await
            .unwrap();
    }

    /// Alice's scope: a person with an alias and a merged duplicate, an
    /// organization, one entity known only from an erased memory, and one
    /// known only from a quarantined one.
    async fn scenario(&self) {
        let m1 = self
            .memory(1, 1_700_000_000, TrustClass::Evidence, "active")
            .await;
        let m2 = self
            .memory(2, 1_700_000_100, TrustClass::Assertion, "active")
            .await;
        let m3 = self
            .memory(3, 1_700_000_050, TrustClass::Assertion, "active")
            .await;
        let erased = self
            .memory(4, 1_700_000_200, TrustClass::Assertion, "erased")
            .await;
        let quarantined = self
            .memory(5, 1_700_000_300, TrustClass::Assertion, "quarantined")
            .await;
        self.entity("ent-dana", "person", "Dana Whitfield", None)
            .await;
        self.alias("ent-dana", "Dana W.").await;
        self.entity("ent-dee", "person", "Dee Whit", Some("ent-dana"))
            .await;
        self.entity("ent-acme", "organization", "Acme Corp", None)
            .await;
        self.entity("ent-zed", "person", "Zed Erased", None).await;
        self.entity("ent-quinn", "person", "Quinn Held", None).await;
        self.mention(m1, "ent-dana", 0, 14).await;
        self.mention(m1, "ent-dana", 30, 34).await;
        self.mention(m2, "ent-dana", 0, 14).await;
        self.mention(m3, "ent-dee", 0, 7).await;
        self.mention(m1, "ent-acme", 20, 29).await;
        self.mention(erased, "ent-zed", 0, 10).await;
        self.mention(quarantined, "ent-quinn", 0, 10).await;
    }

    /// Bob's scope: an entity Alice's queries must never reach.
    async fn others(&self) {
        let memory = self
            .memory(900, 1_700_000_000, TrustClass::Evidence, "active")
            .await;
        self.entity("ent-bob-only", "person", "Bobby Tables", None)
            .await;
        self.mention(memory, "ent-bob-only", 0, 12).await;
    }
}

/// The set as the committed file spells it.
fn describe(set: &PointerSet) -> String {
    let pointers: Vec<Json> = set
        .pointers
        .iter()
        .map(|p| {
            json!({
                "canonical": p.canonical,
                "end": p.end,
                "entity": p.entity.as_str(),
                "kind": p.kind.label(),
                "last_seen": p.last_seen.get(),
                "memories": p.memories,
                "newest": p.newest.to_string(),
                "start": p.start,
                "trust": p.trust,
            })
        })
        .collect();
    let mut text = serde_json::to_string_pretty(&json!({
        "pointers": pointers,
        "text_truncated": set.text_truncated,
        "truncated": set.truncated,
    }))
    .unwrap();
    text.push('\n');
    text
}

async fn resolve(fixture: &Fixture, text: &str, bounds: &PointerBounds) -> PointerSet {
    pointers(&fixture.store, &fixture.scope_id, text, bounds)
        .await
        .unwrap()
}

fn names(set: &PointerSet) -> Vec<&str> {
    set.pointers.iter().map(|p| p.canonical.as_str()).collect()
}

/// FR-001: two known entities named by alias and one unknown name yield
/// exactly the two recorded pointers, in the recorded order, byte for byte.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr001_two_known_names_and_an_unknown_one_yield_the_committed_pointers() {
    let node = common::migrated().await;
    let fixture = Fixture::new(node.handle(), "alice").await;
    fixture.scenario().await;
    let text = "Ask acme   corp about Dana W. and Nobody Known.";
    let first = describe(&resolve(&fixture, text, &PointerBounds::default()).await);
    let second = describe(&resolve(&fixture, text, &PointerBounds::default()).await);
    assert_eq!(
        first, second,
        "B-1: the same rows and text read the same bytes"
    );

    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/pointers/acme-dana.json");
    let committed = fs::read_to_string(&path).unwrap();
    assert_eq!(first, committed, "{}", path.display());
    node.shutdown().await;
}

/// B-1: an exact alias, a normalized form, and a merged duplicate each
/// resolve, the longest name wins, and nothing is created or written.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b1_exact_normalized_and_merged_names_resolve_and_nothing_is_written() {
    let node = common::migrated().await;
    let fixture = Fixture::new(node.handle(), "alice").await;
    fixture.scenario().await;
    let bounds = PointerBounds::default();

    let set = resolve(&fixture, "dana whitfield", &bounds).await;
    assert_eq!(names(&set), ["Dana Whitfield"], "a normalized-form match");
    assert_eq!(
        set.pointers[0].memories, 3,
        "the merged duplicate's mention counts"
    );
    let set = resolve(&fixture, "met Dee Whit today", &bounds).await;
    assert_eq!(
        names(&set),
        ["Dana Whitfield"],
        "a merged entity is presented as its survivor"
    );
    assert_eq!(set.pointers[0].entity.as_str(), "ent-dana");
    let set = resolve(&fixture, "Dana", &bounds).await;
    assert!(set.pointers.is_empty(), "a partial name is not an alias");

    let count = |sql: &'static str| {
        let store = fixture.store.clone();
        async move {
            let rows: Vec<Count> = ReadStore::query(&store, sql, Vec::new()).await.unwrap();
            rows[0].n
        }
    };
    let entities = count("SELECT COUNT(*) AS n FROM entity").await;
    let mentions = count("SELECT COUNT(*) AS n FROM mention").await;
    let aliases = count("SELECT COUNT(*) AS n FROM entity_alias").await;
    resolve(&fixture, "Dana W. Acme Corp Unknown Person", &bounds).await;
    assert_eq!(count("SELECT COUNT(*) AS n FROM entity").await, entities);
    assert_eq!(count("SELECT COUNT(*) AS n FROM mention").await, mentions);
    assert_eq!(
        count("SELECT COUNT(*) AS n FROM entity_alias").await,
        aliases
    );
    node.shutdown().await;
}

/// B-3 and FR-002: an entity whose only mentioning memory is erased, and
/// another whose only one is quarantined, produce no pointer; the live ones
/// count only live memories.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr002_erased_and_quarantined_sources_produce_no_pointer() {
    let node = common::migrated().await;
    let fixture = Fixture::new(node.handle(), "alice").await;
    fixture.scenario().await;
    let set = resolve(
        &fixture,
        "Zed Erased, Quinn Held, and Acme Corp",
        &PointerBounds::default(),
    )
    .await;
    assert_eq!(names(&set), ["Acme Corp"]);

    // A live mention appearing later makes the entity pointable, and counts
    // that memory alone.
    let later = fixture
        .memory(6, 1_700_000_400, TrustClass::Evidence, "active")
        .await;
    fixture.mention(later, "ent-zed", 0, 10).await;
    let set = resolve(&fixture, "Zed Erased", &PointerBounds::default()).await;
    assert_eq!(set.pointers.len(), 1);
    let zed = &set.pointers[0];
    assert_eq!(zed.memories, 1, "the erased memory is not counted");
    assert_eq!(zed.last_seen.get(), 1_700_000_400);
    assert_eq!(zed.trust, "evidence");
    assert_eq!(zed.newest, later);
    node.shutdown().await;
}

/// B-4: the pointer carries the strongest trust class, the newest live
/// memory, and the count of distinct live memories (not of mentions).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b4_counts_distinct_live_memories_with_the_newest_and_strongest() {
    let node = common::migrated().await;
    let fixture = Fixture::new(node.handle(), "alice").await;
    fixture.scenario().await;
    let set = resolve(&fixture, "Acme Corp", &PointerBounds::default()).await;
    let acme = &set.pointers[0];
    assert_eq!((acme.memories, acme.last_seen.get()), (1, 1_700_000_000));
    assert_eq!((acme.start, acme.end), (0, 9));
    assert_eq!(acme.trust, "evidence");
    let set = resolve(&fixture, "Dana Whitfield", &PointerBounds::default()).await;
    let dana = &set.pointers[0];
    assert_eq!(
        dana.memories, 3,
        "m1 mentions her twice and still counts once"
    );
    assert_eq!(dana.last_seen.get(), 1_700_000_100);
    assert_eq!(dana.newest, memory_id(2));
    assert_eq!(dana.trust, "evidence");
    node.shutdown().await;
}

/// B-7 and FR-003: another scope's entity produces no pointer, and no read
/// names a scope other than the one searched.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr003_another_scopes_entity_is_neither_pointed_at_nor_read() {
    let node = common::migrated().await;
    let alice = Fixture::new(node.handle(), "alice").await;
    alice.scenario().await;
    let bob = Fixture::new(node.handle(), "bob").await;
    bob.others().await;
    let recording = Recording {
        inner: node.handle(),
        seen: Mutex::new(Vec::new()),
    };
    let set = pointers(
        &recording,
        &alice.scope_id,
        "Bobby Tables and Acme Corp",
        &PointerBounds::default(),
    )
    .await
    .unwrap();
    assert_eq!(names(&set), ["Acme Corp"]);
    let seen = recording.seen.lock().unwrap().clone();
    assert!(!seen.is_empty(), "the resolver read the store");
    for (sql, values) in &seen {
        assert!(
            sql.contains("scope_id = ?1"),
            "every statement names the scope in its predicate: {sql}"
        );
        assert_eq!(
            values.first(),
            Some(&Value::from(alice.scope_id.as_str())),
            "every statement binds the searched scope first: {sql}"
        );
        let other = Value::from(bob.scope_id.as_str());
        assert!(
            !values.contains(&other),
            "no statement is bound to another scope: {sql}"
        );
    }

    // Asked in bob's own scope, the same name resolves.
    let set = resolve(&bob, "Bobby Tables", &PointerBounds::default()).await;
    assert_eq!(names(&set), ["Bobby Tables"]);
    node.shutdown().await;
}

/// B-6 and FR-005: more entities than the bound are cut from the end in
/// position order, and the set says so.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr005_more_names_than_the_bound_are_cut_from_the_end() {
    let node = common::migrated().await;
    let fixture = Fixture::new(node.handle(), "alice").await;
    let memory = fixture
        .memory(1, 1_700_000_000, TrustClass::Evidence, "active")
        .await;
    for n in 0..8_u32 {
        let id = format!("ent-n{n}");
        fixture
            .entity(
                &id,
                "concept",
                &format!("Topic {}", char::from(b'a' + n as u8)),
                None,
            )
            .await;
        fixture
            .mention(memory, &id, i64::from(n), i64::from(n) + 7)
            .await;
    }
    let text = "Topic h, Topic c, Topic a, Topic b, Topic d, Topic e, Topic f, Topic g";
    let set = resolve(&fixture, text, &PointerBounds::default()).await;
    assert_eq!(
        names(&set),
        ["Topic h", "Topic c", "Topic a", "Topic b", "Topic d"],
        "by first match position, not by name"
    );
    assert!(set.truncated);
    assert!(!set.text_truncated);
    let whole = resolve(&fixture, text, &PointerBounds::new(50, DEFAULT_TEXT_BYTES)).await;
    assert_eq!(whole.pointers.len(), 8);
    assert!(!whole.truncated);
    node.shutdown().await;
}

/// B-6 and FR-009: a text longer than the bound resolves over its prefix
/// only. A name wholly past the bound, and one that straddles it, produce no
/// pointer, and the set says the text was cut.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr009_a_long_text_is_resolved_over_its_prefix_only() {
    let node = common::migrated().await;
    let fixture = Fixture::new(node.handle(), "alice").await;
    fixture.scenario().await;
    let bounds = PointerBounds::default();
    let filler = "x ".repeat(DEFAULT_TEXT_BYTES / 2 - 16);

    // Inside the bound.
    let text = format!("Acme Corp {filler}");
    assert!(text.len() < DEFAULT_TEXT_BYTES);
    let set = resolve(&fixture, &text, &bounds).await;
    assert_eq!(names(&set), ["Acme Corp"]);
    assert!(!set.text_truncated);

    // Wholly past the bound.
    let text = format!("{filler}{filler} Acme Corp");
    assert!(text.len() > DEFAULT_TEXT_BYTES);
    let set = resolve(&fixture, &text, &bounds).await;
    assert!(set.pointers.is_empty());
    assert!(set.text_truncated);
    assert!(!set.truncated, "a cut text is not a cut set");

    // Straddling it: "Acme" ends exactly on the bound, " Corp" lies past it.
    let lead = "y".repeat(DEFAULT_TEXT_BYTES - 5);
    let text = format!("{lead} Acme Corp");
    let set = resolve(&fixture, &text, &bounds).await;
    assert!(set.pointers.is_empty(), "a straddling name is not matched");
    assert!(set.text_truncated);

    // A multibyte character on the bound is cut whole, not split.
    let text = format!("Acme Corp {}\u{20ac}", "z".repeat(DEFAULT_TEXT_BYTES - 11));
    let set = resolve(&fixture, &text, &bounds).await;
    assert_eq!(names(&set), ["Acme Corp"]);
    assert!(set.text_truncated);
    node.shutdown().await;
}

/// B-1: a name inside a longer sentence of unrelated words resolves, an
/// empty text resolves to nothing, and a name shared by two entities resolves
/// to the live one first seen earliest.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b1_empty_text_ambiguous_names_and_unrelated_words() {
    let node = common::migrated().await;
    let fixture = Fixture::new(node.handle(), "alice").await;
    fixture.scenario().await;
    let bounds = PointerBounds::default();
    assert!(resolve(&fixture, "", &bounds).await.pointers.is_empty());
    assert!(
        resolve(&fixture, "?! ...", &bounds)
            .await
            .pointers
            .is_empty()
    );

    // Two entities named "Sam": the earlier has no live mention.
    let live = fixture
        .memory(7, 1_700_000_500, TrustClass::Assertion, "active")
        .await;
    fixture
        .store
        .execute(
            "INSERT INTO entity
             (entity_id, scope_id, kind, canonical, first_seen, last_seen)
             VALUES ('ent-sam-old', ?1, 'person', 'Sam', 1, 1),
                    ('ent-sam-new', ?1, 'person', 'Sam', 2, 2)",
            vec![Value::from(fixture.scope_id.as_str())],
        )
        .await
        .unwrap();
    fixture.alias("ent-sam-old", "Sam").await;
    fixture.alias("ent-sam-new", "Sam").await;
    fixture.mention(live, "ent-sam-new", 0, 3).await;
    let set = resolve(&fixture, "tell Sam", &bounds).await;
    assert_eq!(set.pointers.len(), 1);
    assert_eq!(set.pointers[0].entity.as_str(), "ent-sam-new");

    // An exact alias beats a normalized-form match for the same name (017
    // B-2): "PAT" is exactly the second entity's alias although the first
    // entity, seen earlier, has the same normalized form.
    let pat = fixture
        .memory(8, 1_700_000_600, TrustClass::Assertion, "active")
        .await;
    fixture
        .store
        .execute(
            "INSERT INTO entity
             (entity_id, scope_id, kind, canonical, first_seen, last_seen)
             VALUES ('ent-pat-lower', ?1, 'person', 'pat', 1, 1),
                    ('ent-pat-upper', ?1, 'person', 'PAT', 2, 2)",
            vec![Value::from(fixture.scope_id.as_str())],
        )
        .await
        .unwrap();
    fixture.alias("ent-pat-lower", "pat").await;
    fixture.alias("ent-pat-upper", "PAT").await;
    fixture.mention(pat, "ent-pat-lower", 0, 3).await;
    fixture.mention(pat, "ent-pat-upper", 4, 7).await;
    let set = resolve(&fixture, "PAT", &bounds).await;
    assert_eq!(set.pointers[0].entity.as_str(), "ent-pat-upper");
    let set = resolve(&fixture, "Pat", &bounds).await;
    assert_eq!(
        set.pointers[0].entity.as_str(),
        "ent-pat-lower",
        "no exact alias: the earliest-seen normalized match"
    );
    node.shutdown().await;
}
