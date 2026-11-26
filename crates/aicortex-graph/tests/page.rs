//! The entity page: a computed, cited view of one entity (spec 056 B-1 to B-8,
//! FR-001, FR-002, FR-004, FR-006).
//!
//! The graph is written as rows with fixed ids and times, so the committed
//! page in `testdata/pages/` is the same bytes on every run. Rows are written
//! the way the extraction worker writes them; what the page does with them is
//! under test, not how they got there.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

mod common;

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::sync::Mutex;

use aicortex_graph::{Continuation, EntityId, EntityPage, Fact, FactEnd, PageBounds, entity_page};
use aicortex_store::{ReadStore, ScopeId};
use aicortex_types::{
    Actor, ActorId, Importance, Memory, MemoryBody, MemoryId, MemoryKind, MemoryParts, Provenance,
    Scope, SourceRef, SourceSystem, TrustClass,
};
use rahi_store::{StoreHandle, Value};
use rahi_types::{Error, UnixSeconds};
use serde::de::DeserializeOwned;
use serde_json::{Value as Json, json};

const AS_OF: u64 = 1_700_001_000;

fn memory_id(tail: u32) -> MemoryId {
    MemoryId::parse(&format!("019c4f00-0000-7000-8000-{tail:012}")).unwrap()
}

fn at(seconds: u64) -> UnixSeconds {
    UnixSeconds::new(seconds)
}

/// A store handle that records every statement it is asked to read.
struct Recording {
    inner: StoreHandle,
    seen: Mutex<Vec<String>>,
}

impl ReadStore for Recording {
    fn query<T: DeserializeOwned + Send + 'static>(
        &self,
        sql: &'static str,
        values: Vec<Value>,
    ) -> impl Future<Output = Result<Vec<T>, Error>> + Send {
        self.seen.lock().unwrap().push(sql.to_owned());
        ReadStore::query(&self.inner, sql, values)
    }

    fn query_consistent<T: DeserializeOwned + Send + 'static>(
        &self,
        sql: &'static str,
        values: Vec<Value>,
    ) -> impl Future<Output = Result<Vec<T>, Error>> + Send {
        self.seen.lock().unwrap().push(sql.to_owned());
        ReadStore::query_consistent(&self.inner, sql, values)
    }
}

/// A scope's rows, written directly.
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

    fn memory_record(&self, tail: u32, text: &str, created: u64, trust: TrustClass) -> Memory {
        let time = at(created);
        Memory::new(MemoryParts {
            id: memory_id(tail),
            scope: self.scope.clone(),
            kind: MemoryKind::Observation,
            body: MemoryBody::text(text),
            actor: Actor::human(ActorId::new("tester").unwrap()),
            provenance: Provenance::captured(
                SourceRef::new(SourceSystem::new("test").unwrap()),
                time,
                time,
            ),
            trust,
            importance: Importance::at(time).unwrap(),
            created: time,
        })
    }

    async fn memory(
        &self,
        tail: u32,
        text: &str,
        created: u64,
        trust: TrustClass,
        status: &str,
    ) -> MemoryId {
        let memory = self.memory_record(tail, text, created, trust);
        let record = serde_json::to_string(&memory).unwrap();
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
                    Value::from(record),
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
        if let Some(survivor) = merged_into {
            self.store
                .execute(
                    "INSERT INTO entity_merge
                     (merge_id, scope_id, survivor, absorbed, merged_at, retain_until)
                     VALUES (?1, ?2, ?3, ?4, 1700000300, 1702592300)",
                    vec![
                        Value::from(format!("mrg-{id}")),
                        Value::from(self.scope_id.as_str()),
                        Value::from(survivor),
                        Value::from(id),
                    ],
                )
                .await
                .unwrap();
        }
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
                    Value::from(aicortex_graph::normalize(alias)),
                ],
            )
            .await
            .unwrap();
    }

    async fn mention(&self, memory: MemoryId, entity: &str, start: i64, end: i64, confidence: i64) {
        self.store
            .execute(
                "INSERT INTO mention
                 (scope_id, memory_id, entity_id, byte_start, byte_end, confidence, extractor,
                  extractor_version)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'pattern', 1)",
                vec![
                    Value::from(self.scope_id.as_str()),
                    Value::from(memory.to_string()),
                    Value::from(entity),
                    Value::Integer(start),
                    Value::Integer(end),
                    Value::Integer(confidence),
                ],
            )
            .await
            .unwrap();
    }

    /// An edge from `subject` to an entity (`Ok`) or a literal (`Err`).
    #[allow(clippy::too_many_arguments)]
    async fn edge(
        &self,
        id: &str,
        subject: &str,
        predicate: &str,
        object: Result<&str, &str>,
        confidence: i64,
        window: (i64, Option<i64>),
        status: &str,
        sources: &[MemoryId],
    ) {
        let (entity, literal) = match object {
            Ok(entity) => (Some(entity.to_owned()), None),
            Err(literal) => (None, Some(literal.to_owned())),
        };
        self.store
            .execute(
                "INSERT INTO edge
                 (edge_id, scope_id, subject, predicate, object_entity, object_literal,
                  confidence, extractor, extractor_version, valid_from, valid_until, status,
                  created)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'pattern', 1, ?8, ?9, ?10, 1700000000)",
                vec![
                    Value::from(id),
                    Value::from(self.scope_id.as_str()),
                    Value::from(subject),
                    Value::from(predicate),
                    Value::from(entity),
                    Value::from(literal),
                    Value::Integer(confidence),
                    Value::Integer(window.0),
                    Value::from(window.1),
                    Value::from(status),
                ],
            )
            .await
            .unwrap();
        for source in sources {
            self.store
                .execute(
                    "INSERT INTO edge_source (scope_id, edge_id, memory_id) VALUES (?1, ?2, ?3)",
                    vec![
                        Value::from(self.scope_id.as_str()),
                        Value::from(id),
                        Value::from(source.to_string()),
                    ],
                )
                .await
                .unwrap();
        }
    }

    async fn contradiction(&self, id: &str, a: &str, b: &str, predicate: &str, detected: i64) {
        self.store
            .execute(
                "INSERT INTO contradiction
                 (contradiction_id, scope_id, edge_a, edge_b, subject, predicate, detected)
                 VALUES (?1, ?2, ?3, ?4, 'ent-dana', ?5, ?6)",
                vec![
                    Value::from(id),
                    Value::from(self.scope_id.as_str()),
                    Value::from(a),
                    Value::from(b),
                    Value::from(predicate),
                    Value::Integer(detected),
                ],
            )
            .await
            .unwrap();
    }

    /// The scenario of FR-001: one person, two aliases, a merge, two
    /// predicates that disagree, a literal, an incoming edge, and three edges
    /// that must not appear (an erased source, a superseded edge, and a fact
    /// that stopped holding).
    async fn scenario(&self) {
        let m1 = self
            .memory(
                1,
                "Dana Whitfield works at Acme Corp. Dana prefers dark mode.",
                1_700_000_000,
                TrustClass::Assertion,
                "active",
            )
            .await;
        let m2 = self
            .memory(
                2,
                "Dana Whitfield works at Globex.",
                1_700_000_100,
                TrustClass::Evidence,
                "active",
            )
            .await;
        let m3 = self
            .memory(
                3,
                "Dee Whit started Apollo with Dana Whitfield at the lab.",
                1_700_000_200,
                TrustClass::Assertion,
                "active",
            )
            .await;
        let m4 = self
            .memory(
                4,
                "Dana Whitfield is a member of Acme Corp.",
                1_700_000_050,
                TrustClass::Assertion,
                "erased",
            )
            .await;
        self.entity("ent-dana", "person", "Dana Whitfield", None)
            .await;
        self.alias("ent-dana", "Dana W.").await;
        self.entity("ent-dee", "person", "Dee Whit", Some("ent-dana"))
            .await;
        self.entity("ent-acme", "organization", "Acme Corp", None)
            .await;
        self.entity("ent-globex", "organization", "Globex", None)
            .await;
        self.entity("ent-apollo", "project", "Apollo", None).await;
        self.entity("ent-lab", "place", "The Lab", None).await;
        self.mention(m1, "ent-dana", 0, 14, 900).await;
        self.mention(m1, "ent-dana", 35, 39, 700).await;
        self.mention(m2, "ent-dana", 0, 14, 900).await;
        self.mention(m3, "ent-dee", 0, 7, 800).await;
        self.mention(m3, "ent-dana", 41, 55, 900).await;
        let from = |at: i64| (at, None);
        self.edge(
            "edg-0001",
            "ent-dana",
            "works_at",
            Ok("ent-acme"),
            800,
            from(1_700_000_000),
            "active",
            &[m1],
        )
        .await;
        self.edge(
            "edg-0002",
            "ent-dana",
            "works_at",
            Ok("ent-globex"),
            700,
            from(1_700_000_100),
            "active",
            &[m2],
        )
        .await;
        self.edge(
            "edg-0003",
            "ent-dana",
            "works_on",
            Ok("ent-apollo"),
            900,
            from(1_700_000_200),
            "active",
            &[m3],
        )
        .await;
        self.edge(
            "edg-0004",
            "ent-dana",
            "prefers",
            Err("dark mode"),
            950,
            from(1_700_000_000),
            "active",
            &[m1],
        )
        .await;
        self.edge(
            "edg-0005",
            "ent-apollo",
            "related_to",
            Ok("ent-dana"),
            600,
            from(1_700_000_200),
            "active",
            &[m3],
        )
        .await;
        self.edge(
            "edg-0006",
            "ent-dana",
            "member_of",
            Ok("ent-acme"),
            900,
            from(1_700_000_050),
            "active",
            &[m4],
        )
        .await;
        self.edge(
            "edg-0007",
            "ent-dana",
            "works_at",
            Ok("ent-acme"),
            500,
            from(1_699_999_000),
            "superseded",
            &[m1],
        )
        .await;
        self.edge(
            "edg-0008",
            "ent-dana",
            "decided",
            Err("ship it"),
            500,
            (1_700_000_000, Some(1_700_000_050)),
            "active",
            &[m2],
        )
        .await;
        self.edge(
            "edg-0009",
            "ent-dee",
            "located_in",
            Ok("ent-lab"),
            650,
            from(1_700_000_200),
            "active",
            &[m3],
        )
        .await;
        self.contradiction(
            "ctd-0001",
            "edg-0001",
            "edg-0002",
            "works_at",
            1_700_000_100,
        )
        .await;
    }
}

fn describe_fact(fact: &Fact) -> Json {
    let other = match &fact.other {
        FactEnd::Entity { id, name } => json!({ "entity": { "id": id.as_str(), "name": name } }),
        FactEnd::Literal(text) => json!({ "literal": text }),
    };
    json!({
        "edge": fact.edge.as_str(),
        "direction": fact.direction.label(),
        "other": other,
        "confidence": fact.confidence,
        "extractor": fact.extractor,
        "extractor_version": fact.extractor_version,
        "valid_from": fact.valid_from.get(),
        "valid_until": fact.valid_until.map(UnixSeconds::get),
        "trust": fact.trust,
        "sources": fact.sources.iter().map(ToString::to_string).collect::<Vec<_>>(),
        "source_count": fact.source_count,
    })
}

/// The page as committed text: every field but the memory bodies, which a
/// surface envelopes and this crate never serializes.
fn describe(page: &EntityPage) -> String {
    let entity = &page.header.entity;
    let value = json!({
        "as_of": page.as_of.get(),
        "header": {
            "id": entity.id.as_str(),
            "kind": entity.kind.label(),
            "canonical": entity.canonical,
            "aliases": entity.aliases,
            "first_seen": entity.first_seen.get(),
            "last_seen": entity.last_seen.get(),
            "mention_count": entity.mention_count,
            "merged": page.header.merged.iter().map(EntityId::as_str).collect::<Vec<_>>(),
        },
        "facts": page.facts.iter().map(|group| json!({
            "predicate": group.predicate.as_str(),
            "facts": group.facts.iter().map(describe_fact).collect::<Vec<_>>(),
            "next": group.next.as_ref().map(ToString::to_string),
        })).collect::<Vec<_>>(),
        "contradictions": {
            "items": page.contradictions.items.iter().map(|c| json!({
                "id": c.id,
                "predicate": c.predicate.as_str(),
                "detected": c.detected.get(),
                "status": c.status,
                "sides": c.sides.iter().map(describe_fact).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
            "next": page.contradictions.next.as_ref().map(ToString::to_string),
        },
        "mentions": {
            "items": page.mentions.items.iter().map(|m| json!({
                "id": m.memory.id.to_string(),
                "created": m.memory.created.get(),
                "trust": m.memory.trust.label(),
                "origin_erased": m.origin_erased,
                "ranges": m.ranges.iter()
                    .map(|r| json!([r.start, r.end, r.confidence]))
                    .collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
            "next": page.mentions.next.as_ref().map(ToString::to_string),
        },
    });
    let mut text = serde_json::to_string_pretty(&value).unwrap();
    text.push('\n');
    text
}

async fn page(store: &StoreHandle, scope_id: &str, id: &str, bounds: &PageBounds) -> EntityPage {
    entity_page(store, scope_id, &EntityId::new(id), at(AS_OF), bounds)
        .await
        .unwrap()
        .unwrap_or_else(|| panic!("no entity {id}"))
}

fn fact_edges(page: &EntityPage) -> Vec<String> {
    page.facts
        .iter()
        .flat_map(|group| group.facts.iter().map(|f| f.edge.to_string()))
        .collect()
}

/// FR-001 and B-2: the scenario produces the committed page byte for byte,
/// twice over.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr001_the_fixture_scope_produces_the_committed_page() {
    let node = common::migrated().await;
    let fixture = Fixture::new(node.handle(), "alice").await;
    fixture.scenario().await;
    let bounds = PageBounds::default();
    let first = describe(&page(&fixture.store, &fixture.scope_id, "ent-dana", &bounds).await);
    let second = describe(&page(&fixture.store, &fixture.scope_id, "ent-dana", &bounds).await);
    assert_eq!(first, second, "B-2: the same state reads the same bytes");

    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/pages/dana.json");
    if std::env::var_os("AICORTEX_BLESS_PAGES").is_some() {
        fs::write(&path, &first).unwrap();
    }
    let committed = fs::read_to_string(&path).unwrap();
    assert_eq!(first, committed, "{}", path.display());
    node.shutdown().await;
}

/// B-3, B-4, B-5 over the scenario: what is shown, what is not, and the order.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b3_to_b5_sections_citations_and_order() {
    let node = common::migrated().await;
    let fixture = Fixture::new(node.handle(), "alice").await;
    fixture.scenario().await;
    let page = page(
        &fixture.store,
        &fixture.scope_id,
        "ent-dana",
        &PageBounds::default(),
    )
    .await;

    // B-5: within `works_at` the evidence-backed fact sorts above the
    // assertion-backed one although its confidence is lower.
    assert_eq!(
        fact_edges(&page),
        [
            "edg-0003", "edg-0002", "edg-0001", "edg-0009", "edg-0005", "edg-0004"
        ],
        "an erased source, a superseded edge, and an ended fact are absent (B-4)"
    );
    let works_at = &page.facts[1];
    assert_eq!(works_at.predicate.as_str(), "works_at");
    assert_eq!(works_at.facts[0].trust, "evidence");
    assert_eq!(works_at.facts[1].trust, "assertion");

    // B-3: the merge, the incoming edge folded under the survivor, and the
    // contradiction with both sides and no winner.
    assert_eq!(page.header.merged, [EntityId::new("ent-dee")]);
    assert_eq!(page.header.entity.aliases, ["Dana W.", "Dee Whit"]);
    let incoming = page
        .facts
        .iter()
        .find(|g| g.predicate.as_str() == "related_to");
    assert_eq!(incoming.unwrap().facts[0].direction.label(), "incoming");
    assert_eq!(page.contradictions.items.len(), 1);
    let sides: Vec<_> = page.contradictions.items[0]
        .sides
        .iter()
        .map(|s| s.edge.to_string())
        .collect();
    assert_eq!(sides, ["edg-0001", "edg-0002"]);

    // B-5: mentions are newest first, with every byte range.
    let ids: Vec<_> = page
        .mentions
        .items
        .iter()
        .map(|m| m.memory.id.to_string())
        .collect();
    assert_eq!(
        ids,
        [memory_id(3), memory_id(2), memory_id(1)].map(|id| id.to_string())
    );
    assert_eq!(page.mentions.items[2].ranges.len(), 2);

    // A merged id reads as its survivor; another scope's id reads as absent.
    let via_merged = entity_page(
        &fixture.store,
        &fixture.scope_id,
        &EntityId::new("ent-dee"),
        at(AS_OF),
        &PageBounds::default(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(via_merged.header.entity.id.as_str(), "ent-dana");
    let (_, other_scope) = common::scope("bob");
    assert!(
        entity_page(
            &fixture.store,
            &other_scope,
            &EntityId::new("ent-dana"),
            at(AS_OF),
            &PageBounds::default(),
        )
        .await
        .unwrap()
        .is_none()
    );
    node.shutdown().await;
}

/// B-1: `as_of` bounds what the page shows by time.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b1_as_of_hides_what_did_not_yet_exist() {
    let node = common::migrated().await;
    let fixture = Fixture::new(node.handle(), "alice").await;
    fixture.scenario().await;
    let early = entity_page(
        &fixture.store,
        &fixture.scope_id,
        &EntityId::new("ent-dana"),
        at(1_700_000_050),
        &PageBounds::default(),
    )
    .await
    .unwrap()
    .unwrap();
    // Only the first memory existed, and the ended fact (valid until 1700000050,
    // exclusive) no longer holds.
    assert_eq!(fact_edges(&early), ["edg-0001", "edg-0004"]);
    assert_eq!(early.mentions.items.len(), 1);
    assert!(early.contradictions.items.is_empty(), "not yet detected");
    node.shutdown().await;
}

/// FR-002 and B-4: after the only memory behind a fact is erased the fact is
/// gone, before and after 017's propagation reaches the edge.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr002_a_fact_with_no_live_source_is_absent_before_and_after_propagation() {
    let node = common::migrated().await;
    let fixture = Fixture::new(node.handle(), "alice").await;
    fixture.scenario().await;
    let store = &fixture.store;
    let read =
        || async { page(store, &fixture.scope_id, "ent-dana", &PageBounds::default()).await };

    // The positive control: its source stands, so the fact is cited.
    let before = read().await;
    let works_on = before
        .facts
        .iter()
        .find(|g| g.predicate.as_str() == "works_on")
        .expect("works_on is on the page");
    assert_eq!(works_on.facts[0].sources, [memory_id(3)]);

    // The erasure transaction has tombstoned the memory; the edge row and its
    // source row are still there (the window of B-4).
    store
        .execute(
            "UPDATE memory SET status = 'erased' WHERE id = ?1",
            vec![Value::from(memory_id(3).to_string())],
        )
        .await
        .unwrap();
    let window = read().await;
    assert!(!fact_edges(&window).contains(&"edg-0003".to_owned()));
    assert!(!fact_edges(&window).contains(&"edg-0009".to_owned()));
    assert!(
        window
            .mentions
            .items
            .iter()
            .all(|m| m.memory.id != memory_id(3))
    );

    // Propagation, as 014's sweep stages it: the source and mention rows go and
    // the edge is marked.
    for sql in [
        "DELETE FROM edge_source WHERE memory_id = ?1",
        "DELETE FROM mention WHERE memory_id = ?1",
        "UPDATE edge SET status = 'origin_erased'
         WHERE NOT EXISTS (SELECT 1 FROM edge_source s WHERE s.edge_id = edge.edge_id)",
    ] {
        store
            .execute(
                sql,
                if sql.contains("?1") {
                    vec![Value::from(memory_id(3).to_string())]
                } else {
                    vec![]
                },
            )
            .await
            .unwrap();
    }
    let after = read().await;
    assert_eq!(fact_edges(&after), fact_edges(&window));
    node.shutdown().await;
}

/// B-4: a superseded source stops supporting the fact, and a fact keeps the
/// sources that remain.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b4_a_fact_keeps_only_the_sources_that_are_still_live() {
    let node = common::migrated().await;
    let fixture = Fixture::new(node.handle(), "alice").await;
    fixture.scenario().await;
    let extra = fixture
        .memory(
            9,
            "Dana Whitfield works at Globex, again.",
            1_700_000_300,
            TrustClass::Assertion,
            "superseded",
        )
        .await;
    fixture
        .store
        .execute(
            "INSERT INTO edge_source (scope_id, edge_id, memory_id) VALUES (?1, 'edg-0002', ?2)",
            vec![
                Value::from(fixture.scope_id.as_str()),
                Value::from(extra.to_string()),
            ],
        )
        .await
        .unwrap();
    let page = page(
        &fixture.store,
        &fixture.scope_id,
        "ent-dana",
        &PageBounds::default(),
    )
    .await;
    let globex = &page.facts[1].facts[0];
    assert_eq!(globex.edge.as_str(), "edg-0002");
    assert_eq!(globex.sources, [memory_id(2)]);
    assert_eq!(globex.source_count, 1);
    node.shutdown().await;
}

/// FR-004 and B-6: every truncated section resumes without a gap or a repeat.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr004_continuations_resume_without_gaps_or_duplicates() {
    let node = common::migrated().await;
    let fixture = Fixture::new(node.handle(), "alice").await;
    fixture
        .entity("ent-dana", "person", "Dana Whitfield", None)
        .await;
    let mut sources = Vec::new();
    for n in 1..=7_u32 {
        let id = fixture
            .memory(
                n,
                &format!("Dana Whitfield prefers option {n}."),
                1_700_000_000 + u64::from(n % 3),
                if n % 2 == 0 {
                    TrustClass::Evidence
                } else {
                    TrustClass::Assertion
                },
                "active",
            )
            .await;
        fixture.mention(id, "ent-dana", 0, 14, 900).await;
        fixture
            .edge(
                &format!("edg-{n:04}"),
                "ent-dana",
                "prefers",
                Err(&format!("option {n}")),
                i64::from(500 + 100 * (n % 3)),
                (1_700_000_000, None),
                "active",
                &[id],
            )
            .await;
        sources.push(id);
    }
    for (n, (a, b)) in [(1, 2), (2, 3), (3, 4), (4, 5)].into_iter().enumerate() {
        fixture
            .contradiction(
                &format!("ctd-{n}"),
                &format!("edg-{a:04}"),
                &format!("edg-{b:04}"),
                "prefers",
                1_700_000_100 + i64::try_from(n % 2).unwrap(),
            )
            .await;
    }
    let store = &fixture.store;
    let scope_id = &fixture.scope_id;
    let whole = page(store, scope_id, "ent-dana", &PageBounds::default()).await;
    assert!(whole.facts[0].next.is_none() && whole.mentions.next.is_none());
    let want_facts = fact_edges(&whole);
    let want_mentions: Vec<_> = whole.mentions.items.iter().map(|m| m.memory.id).collect();
    let want_contradictions: Vec<_> = whole
        .contradictions
        .items
        .iter()
        .map(|c| c.id.clone())
        .collect();
    assert_eq!(want_facts.len(), 7);
    assert_eq!(want_mentions.len(), 7);
    assert_eq!(want_contradictions.len(), 4);

    let narrow = PageBounds {
        facts_per_predicate: 3,
        contradictions: 3,
        mentions: 3,
        resume: None,
    };
    let first = page(store, scope_id, "ent-dana", &narrow).await;
    assert_eq!(first.facts[0].facts.len(), 3);
    assert_eq!(first.contradictions.items.len(), 3);
    assert_eq!(first.mentions.items.len(), 3);

    let mut facts = fact_edges(&first);
    let mut token = first.facts[0].next.clone();
    let mut mentions: Vec<_> = first.mentions.items.iter().map(|m| m.memory.id).collect();
    let mut mention_token = first.mentions.next.clone();
    let mut contradictions: Vec<_> = first
        .contradictions
        .items
        .iter()
        .map(|c| c.id.clone())
        .collect();
    let mut contradiction_token = first.contradictions.next.clone();
    while let Some(next) = token {
        // The token survives its text form, which is how a client returns it.
        let resume: Continuation = next.to_string().parse().unwrap();
        let slice = page(
            store,
            scope_id,
            "ent-dana",
            &PageBounds {
                resume: Some(resume),
                ..narrow.clone()
            },
        )
        .await;
        assert!(slice.mentions.items.is_empty() && slice.contradictions.items.is_empty());
        facts.extend(fact_edges(&slice));
        token = slice.facts.first().and_then(|g| g.next.clone());
    }
    while let Some(next) = mention_token {
        let slice = page(
            store,
            scope_id,
            "ent-dana",
            &PageBounds {
                resume: Some(next.to_string().parse().unwrap()),
                ..narrow.clone()
            },
        )
        .await;
        assert!(slice.facts.is_empty());
        mentions.extend(slice.mentions.items.iter().map(|m| m.memory.id));
        mention_token = slice.mentions.next.clone();
    }
    while let Some(next) = contradiction_token {
        let slice = page(
            store,
            scope_id,
            "ent-dana",
            &PageBounds {
                resume: Some(next.to_string().parse().unwrap()),
                ..narrow.clone()
            },
        )
        .await;
        contradictions.extend(slice.contradictions.items.iter().map(|c| c.id.clone()));
        contradiction_token = slice.contradictions.next.clone();
    }
    assert_eq!(facts, want_facts);
    assert_eq!(mentions, want_mentions);
    assert_eq!(contradictions, want_contradictions);
    assert_eq!(
        facts.iter().collect::<BTreeSet<_>>().len(),
        facts.len(),
        "no repeat"
    );

    for bad in [
        "",
        "facts",
        "facts:no_such:1:1:e",
        "mentions:x:y",
        "other:1:2",
    ] {
        let error = bad.parse::<Continuation>().unwrap_err();
        assert!(
            error.to_string().contains("not a token this page issued"),
            "{bad:?}: {error}"
        );
    }
    node.shutdown().await;
}

/// FR-006 and B-1: building a page reads and nothing else.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr006_a_page_read_issues_only_selects_and_changes_no_row() {
    let node = common::migrated().await;
    let fixture = Fixture::new(node.handle(), "alice").await;
    fixture.scenario().await;
    let tables = [
        "entity",
        "entity_alias",
        "entity_merge",
        "mention",
        "edge",
        "edge_source",
        "contradiction",
        "memory",
    ];
    let snapshot = || async {
        let mut counts = Vec::new();
        for table in tables {
            let sql = format!("SELECT COUNT(*) AS count FROM {table}");
            counts.push(common::count(&fixture.store, &sql, vec![]).await);
        }
        counts
    };
    let before = snapshot().await;
    let recording = Recording {
        inner: fixture.store.clone(),
        seen: Mutex::new(Vec::new()),
    };
    entity_page(
        &recording,
        &fixture.scope_id,
        &EntityId::new("ent-dana"),
        at(AS_OF),
        &PageBounds::default(),
    )
    .await
    .unwrap()
    .unwrap();
    {
        let seen = recording.seen.lock().unwrap();
        assert!(!seen.is_empty());
        for sql in seen.iter() {
            assert!(
                sql.trim_start().to_ascii_uppercase().starts_with("SELECT"),
                "a page read issued a non-select: {sql}"
            );
        }
    }
    assert_eq!(snapshot().await, before, "no row changed");

    // The crate that builds the page opens no socket and calls no kernel: it
    // does not depend on the egress facade or on any HTTP client.
    let manifest =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml")).unwrap();
    for egress in [
        "rahi-kernel",
        "reqwest",
        "hyper",
        "ureq",
        "tokio-tungstenite",
    ] {
        assert!(!manifest.contains(egress), "{egress} is an egress path");
    }
    node.shutdown().await;
}
