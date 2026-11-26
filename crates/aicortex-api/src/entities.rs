//! `GET /entities`, `GET /entities/{id}`, and `GET /entities/{id}/neighbors`
//! (B-4, 020 D-3, 017 D-9, 056 B-9).
//!
//! Both routes resolve the scope against the caller first (B-2), so a scope
//! the caller cannot reach is a 404 decided before any store read. They read
//! the derived graph of `aicortex-graph` and add no graph logic of their own.
//!
//! An entity is derived, never authoritative (017 B-6), and its names come
//! from stored text. Every response therefore carries the framing statement
//! of 019 beside the names and says it is derived, and the walk returns
//! memory ids with their paths and never a memory body: content leaves only
//! through the envelopes of `/memories`.
//!
//! The page of `GET /entities/{id}` is the one route here that returns content
//! derived from memories, and it returns it only through 019: every mentioning
//! memory through `frame_memories`, every literal edge object through
//! `frame_literal`, both after the versioned framing statement (056 B-7, B-8).

use std::collections::BTreeSet;

use aicortex_graph::{
    Continuation, DEFAULT_DEPTH, DEFAULT_FANOUT, Entity, EntityId, EntityKind, EntityPage, Fact,
    FactEnd, Hop, HopEnd, MAX_LIST, PageBounds, Predicate, WalkFilter, WalkHit, WalkOptions,
};
use aicortex_recall::{
    EnvelopeInput, FRAMING_STATEMENT, FRAMING_VERSION, FramedMemories, LiteralEnvelope,
    LiteralInput, frame_literal, frame_memories,
};
use aicortex_store::ScopeId;
use axum::Json;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use rahi_types::UnixSeconds;
use serde::{Deserialize, Serialize};

use crate::capture::now;
use crate::dto::ScopeRef;
use crate::error::{ApiError, ApiResult};
use crate::router::{Api, Caller};

/// Entities per page when the request names no limit.
pub const DEFAULT_PAGE: u32 = 50;

/// `GET /entities`: a scope, an optional kind, and a keyset page.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EntitiesQuery {
    /// `personal` (the default), `project`, or `shared`.
    #[serde(default)]
    pub scope: Option<String>,
    /// The project key.
    #[serde(default)]
    pub project: Option<String>,
    /// The share key.
    #[serde(default)]
    pub share: Option<String>,
    /// The owning subject; the caller when absent.
    #[serde(default)]
    pub owner: Option<String>,
    /// Only entities of this kind.
    #[serde(default)]
    pub kind: Option<String>,
    /// The id the previous page ended at.
    #[serde(default)]
    pub cursor: Option<String>,
    /// Rows per page.
    #[serde(default)]
    pub limit: Option<u32>,
}

/// `GET /entities/{id}/neighbors`: a scope and the bounds of the walk.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NeighborsQuery {
    /// `personal` (the default), `project`, or `shared`.
    #[serde(default)]
    pub scope: Option<String>,
    /// The project key.
    #[serde(default)]
    pub project: Option<String>,
    /// The share key.
    #[serde(default)]
    pub share: Option<String>,
    /// The owning subject; the caller when absent.
    #[serde(default)]
    pub owner: Option<String>,
    /// Hops to follow; clamped to the walk's maximum.
    #[serde(default)]
    pub depth: Option<u8>,
    /// Edges followed per node per hop; clamped to the walk's maximum.
    #[serde(default)]
    pub fanout: Option<u16>,
    /// Comma separated predicates to follow; all when absent.
    #[serde(default)]
    pub predicates: Option<String>,
    /// Only edges at or above this confidence, in permille.
    #[serde(default)]
    pub min_confidence: Option<u16>,
}

fn scope_ref(
    scope: &Option<String>,
    project: &Option<String>,
    share: &Option<String>,
    owner: &Option<String>,
) -> ScopeRef {
    ScopeRef {
        kind: scope.clone().unwrap_or_else(|| "personal".to_owned()),
        project: project.clone(),
        share: share.clone(),
        owner: owner.clone(),
    }
}

/// One entity as the API shows it.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct EntityOut {
    /// Its id.
    pub id: String,
    /// What it is.
    pub kind: &'static str,
    /// The name it is shown under.
    pub canonical: String,
    /// Its other names.
    pub aliases: Vec<String>,
    /// The earliest mention, in Unix seconds.
    pub first_seen: u64,
    /// The latest mention, in Unix seconds.
    pub last_seen: u64,
    /// How many mentions point at it.
    pub mention_count: u64,
}

impl EntityOut {
    fn of(entity: Entity) -> Self {
        Self {
            id: entity.id.to_string(),
            kind: entity.kind.label(),
            canonical: entity.canonical,
            aliases: entity.aliases,
            first_seen: entity.first_seen.get(),
            last_seen: entity.last_seen.get(),
            mention_count: entity.mention_count,
        }
    }
}

/// The answer to `GET /entities`.
#[derive(Debug, Serialize)]
pub struct EntitiesResponse {
    /// The framing statement of 019: the names below are recalled data.
    pub framing: &'static str,
    /// Always true: an entity is derived and never outranks its memories.
    pub derived: bool,
    /// The page.
    pub items: Vec<EntityOut>,
    /// The cursor of the next page, when there may be one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
}

/// Where a hop arrived.
#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HopEndOut {
    /// Another entity.
    Entity(String),
    /// A literal object.
    Literal(String),
}

/// One step of a path.
#[derive(Debug, Serialize)]
pub struct HopOut {
    /// The entity the hop left.
    pub from: String,
    /// The edge followed.
    pub edge: String,
    /// Its predicate.
    pub predicate: &'static str,
    /// Where it arrived.
    pub to: HopEndOut,
}

impl HopOut {
    fn of(hop: &Hop) -> Self {
        Self {
            from: hop.from.to_string(),
            edge: hop.edge.to_string(),
            predicate: hop.predicate.as_str(),
            to: match &hop.to {
                HopEnd::Entity(id) => HopEndOut::Entity(id.to_string()),
                HopEnd::Literal(text) => HopEndOut::Literal(text.clone()),
            },
        }
    }
}

/// A memory the walk reached, and how: an id and a path, never a body.
#[derive(Debug, Serialize)]
pub struct HitOut {
    /// The memory; read it through `/memories/{id}` for its envelope.
    pub memory: String,
    /// The hops that reached it.
    pub path: Vec<HopOut>,
}

impl HitOut {
    fn of(hit: &WalkHit) -> Self {
        Self {
            memory: hit.memory.to_string(),
            path: hit.path.0.iter().map(HopOut::of).collect(),
        }
    }
}

/// The answer to `GET /entities/{id}/neighbors`.
#[derive(Debug, Serialize)]
pub struct NeighborsResponse {
    /// The framing statement of 019.
    pub framing: &'static str,
    /// Always true: a walk routes to memories and is no answer itself.
    pub derived: bool,
    /// The entity the walk started from, after following a merge.
    pub entity: String,
    /// The memories reached, each with its path.
    pub hits: Vec<HitOut>,
}

/// The query string, or a problem document saying why it is not one.
fn query<T>(query: Result<Query<T>, QueryRejection>) -> ApiResult<T> {
    query
        .map(|Query(value)| value)
        .map_err(|rejection| ApiError::invalid(rejection.body_text()))
}

/// `GET /entities`: one keyset page in id order.
pub async fn list(
    State(api): State<Api>,
    caller: Caller,
    request: Result<Query<EntitiesQuery>, QueryRejection>,
) -> ApiResult<Json<EntitiesResponse>> {
    let request = query(request)?;
    let scope = scope_ref(
        &request.scope,
        &request.project,
        &request.share,
        &request.owner,
    )
    .resolve(caller.sub())?;
    let kind = request
        .kind
        .as_deref()
        .map(|label| {
            EntityKind::parse(label)
                .ok_or_else(|| ApiError::invalid(format!("kind {label:?} is not an entity kind")))
        })
        .transpose()?;
    let limit = request.limit.unwrap_or(DEFAULT_PAGE).clamp(1, MAX_LIST);
    let scope_id = ScopeId::of(&scope);
    let entities = aicortex_graph::list(
        api.app.store(),
        scope_id.as_str(),
        kind,
        request.cursor.as_deref(),
        limit,
    )
    .await?;
    let next = (entities.len() == usize::try_from(limit).unwrap_or(usize::MAX))
        .then(|| entities.last().map(|entity| entity.id.to_string()))
        .flatten();
    Ok(Json(EntitiesResponse {
        framing: FRAMING_STATEMENT,
        derived: true,
        items: entities.into_iter().map(EntityOut::of).collect(),
        next,
    }))
}

/// `GET /entities/{id}/neighbors`: a bounded walk that returns memory ids and
/// the path to each (017 B-9).
pub async fn neighbors(
    State(api): State<Api>,
    caller: Caller,
    Path(id): Path<String>,
    request: Result<Query<NeighborsQuery>, QueryRejection>,
) -> ApiResult<Json<NeighborsResponse>> {
    let request = query(request)?;
    let scope = scope_ref(
        &request.scope,
        &request.project,
        &request.share,
        &request.owner,
    )
    .resolve(caller.sub())?;
    let predicates = request
        .predicates
        .as_deref()
        .map(|list| {
            list.split(',')
                .map(|text| {
                    Predicate::parse(text.trim()).ok_or_else(|| {
                        ApiError::invalid(format!("predicate {text:?} is not in the vocabulary"))
                    })
                })
                .collect::<ApiResult<BTreeSet<_>>>()
        })
        .transpose()?;
    let options = WalkOptions {
        depth: request.depth.unwrap_or(DEFAULT_DEPTH),
        fanout: request.fanout.unwrap_or(DEFAULT_FANOUT),
        filter: WalkFilter {
            predicates,
            min_confidence: request.min_confidence.unwrap_or(0),
        },
    };
    let scope_id = ScopeId::of(&scope);
    let start = aicortex_graph::get(api.app.store(), scope_id.as_str(), &EntityId::new(id))
        .await?
        .ok_or_else(ApiError::not_found)?;
    let hits =
        aicortex_graph::neighbors(api.app.store(), scope_id.as_str(), &start.id, &options).await?;
    Ok(Json(NeighborsResponse {
        framing: FRAMING_STATEMENT,
        derived: true,
        entity: start.id.to_string(),
        hits: hits.iter().map(HitOut::of).collect(),
    }))
}

/// `GET /entities/{id}`: a scope, a moment, and the bounds of each section.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PageQuery {
    /// `personal` (the default), `project`, or `shared`.
    #[serde(default)]
    pub scope: Option<String>,
    /// The project key.
    #[serde(default)]
    pub project: Option<String>,
    /// The share key.
    #[serde(default)]
    pub share: Option<String>,
    /// The owning subject; the caller when absent.
    #[serde(default)]
    pub owner: Option<String>,
    /// Read the page as of this Unix time; the moment of the request when
    /// absent (056 D-3).
    #[serde(default)]
    pub as_of: Option<u64>,
    /// Facts per predicate; clamped to the page's maximum.
    #[serde(default)]
    pub facts: Option<u32>,
    /// Contradictions; clamped to the page's maximum.
    #[serde(default)]
    pub contradictions: Option<u32>,
    /// Mentioning memories; clamped to the page's maximum.
    #[serde(default)]
    pub mentions: Option<u32>,
    /// A `next` value a previous page returned, to resume that section alone.
    #[serde(default)]
    pub after: Option<String>,
}

/// The far end of a fact. A literal is an envelope, never bare text (056 B-7).
#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OtherOut {
    /// Another entity; its name is a derived value.
    Entity {
        /// Its id.
        id: String,
        /// The name it is shown under.
        name: String,
    },
    /// Literal text extracted from a memory, in its envelope.
    Literal(LiteralEnvelope),
}

/// One cited fact.
#[derive(Serialize)]
pub struct FactOut {
    /// The edge.
    pub edge: String,
    /// `outgoing` when the entity is the edge's subject, else `incoming`.
    pub direction: &'static str,
    /// The other end.
    pub other: OtherOut,
    /// The extractor's confidence in permille.
    pub confidence: u16,
    /// The extractor.
    pub extractor: String,
    /// The extractor's version.
    pub extractor_version: u32,
    /// When the fact began to hold, in Unix seconds.
    pub valid_from: u64,
    /// When it stopped holding, if it did.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub valid_until: Option<u64>,
    /// The strongest trust class among its live sources.
    pub trust: &'static str,
    /// Its live source memories; read them through `/memories/{id}`.
    pub sources: Vec<String>,
    /// How many live source memories it has in all.
    pub source_count: u64,
}

impl FactOut {
    fn of(fact: &Fact) -> ApiResult<Self> {
        let other = match &fact.other {
            FactEnd::Entity { id, name } => OtherOut::Entity {
                id: id.to_string(),
                name: name.clone(),
            },
            FactEnd::Literal(text) => OtherOut::Literal(
                frame_literal(LiteralInput::new(
                    fact.edge.as_str(),
                    text,
                    fact.trust,
                    &fact.sources,
                ))
                .map_err(|_| enveloping_failed("a literal"))?,
            ),
        };
        Ok(Self {
            edge: fact.edge.to_string(),
            direction: fact.direction.label(),
            other,
            confidence: fact.confidence,
            extractor: fact.extractor.clone(),
            extractor_version: fact.extractor_version,
            valid_from: fact.valid_from.get(),
            valid_until: fact.valid_until.map(UnixSeconds::get),
            trust: fact.trust,
            sources: fact.sources.iter().map(ToString::to_string).collect(),
            source_count: fact.source_count,
        })
    }
}

/// The facts of one predicate.
#[derive(Serialize)]
pub struct FactGroupOut {
    /// The predicate.
    pub predicate: &'static str,
    /// The facts, strongest first.
    pub facts: Vec<FactOut>,
    /// Pass as `after` to read the rest of this group.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
}

/// One open contradiction: every side, and no winner.
#[derive(Serialize)]
pub struct ContradictionOut {
    /// Its id, which names the review item it waits on.
    pub id: String,
    /// The shared predicate.
    pub predicate: &'static str,
    /// When it was detected, in Unix seconds.
    pub detected: u64,
    /// Its status.
    pub status: String,
    /// The disagreeing facts.
    pub sides: Vec<FactOut>,
}

/// The contradictions section.
#[derive(Serialize)]
pub struct ContradictionsOut {
    /// The contradictions on this slice.
    pub items: Vec<ContradictionOut>,
    /// Pass as `after` to read the rest.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
}

/// Where a memory mentions the entity.
#[derive(Debug, Serialize)]
pub struct RangeOut {
    /// The first byte.
    pub start: u64,
    /// One past the last byte.
    pub end: u64,
    /// The extractor's confidence in permille.
    pub confidence: u16,
}

/// The bare fields of one mentioning memory, in envelope order.
#[derive(Debug, Serialize)]
pub struct MentionOut {
    /// The memory.
    pub id: String,
    /// When it was created, in Unix seconds.
    pub created: u64,
    /// Where it mentions the entity.
    pub ranges: Vec<RangeOut>,
}

/// The mentions section: envelopes, then their bare fields.
#[derive(Serialize)]
pub struct MentionsOut {
    /// The framing statement and the envelopes, as 019 serializes them.
    #[serde(flatten)]
    pub framed: FramedMemories,
    /// The bare fields of each memory, in envelope order.
    pub items: Vec<MentionOut>,
    /// Pass as `after` to read the rest.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
}

/// The header of a page.
#[derive(Serialize)]
pub struct HeaderOut {
    /// The entity, after following a merge.
    #[serde(flatten)]
    pub entity: EntityOut,
    /// The ids of entities merged into this one.
    pub merged: Vec<String>,
}

/// The answer to `GET /entities/{id}` (056 B-3, B-7, B-8).
#[derive(Serialize)]
pub struct EntityPageResponse {
    /// The version of the framing statement, ahead of every envelope.
    pub framing_version: u32,
    /// The framing statement of 019: what follows is recalled data.
    pub framing: &'static str,
    /// Always true: a page is derived, grants no trust to what it gathers, and
    /// every name on it is a derived value.
    pub derived: bool,
    /// The moment the page was read as of, in Unix seconds.
    pub as_of: u64,
    /// Section 1.
    pub header: HeaderOut,
    /// Section 2: groups in the predicate vocabulary's order.
    pub facts: Vec<FactGroupOut>,
    /// Section 3.
    pub contradictions: ContradictionsOut,
    /// Section 4.
    pub mentions: MentionsOut,
}

fn enveloping_failed(what: &str) -> ApiError {
    ApiError::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal",
        format!("{what} could not be enveloped"),
    )
}

fn next_of(token: Option<&Continuation>) -> Option<String> {
    token.map(ToString::to_string)
}

impl EntityPageResponse {
    fn of(page: &EntityPage) -> ApiResult<Self> {
        let facts = page
            .facts
            .iter()
            .map(|group| {
                Ok(FactGroupOut {
                    predicate: group.predicate.as_str(),
                    facts: group
                        .facts
                        .iter()
                        .map(FactOut::of)
                        .collect::<ApiResult<_>>()?,
                    next: next_of(group.next.as_ref()),
                })
            })
            .collect::<ApiResult<_>>()?;
        let contradictions = ContradictionsOut {
            items: page
                .contradictions
                .items
                .iter()
                .map(|found| {
                    Ok(ContradictionOut {
                        id: found.id.clone(),
                        predicate: found.predicate.as_str(),
                        detected: found.detected.get(),
                        status: found.status.clone(),
                        sides: found
                            .sides
                            .iter()
                            .map(FactOut::of)
                            .collect::<ApiResult<_>>()?,
                    })
                })
                .collect::<ApiResult<_>>()?,
            next: next_of(page.contradictions.next.as_ref()),
        };
        let framed = frame_memories(
            page.mentions
                .items
                .iter()
                .map(|found| EnvelopeInput::new(&found.memory, found.origin_erased)),
        )
        .map_err(|_| enveloping_failed("a memory"))?;
        let mentions = MentionsOut {
            framed,
            items: page
                .mentions
                .items
                .iter()
                .map(|found| MentionOut {
                    id: found.memory.id.to_string(),
                    created: found.memory.created.get(),
                    ranges: found
                        .ranges
                        .iter()
                        .map(|range| RangeOut {
                            start: range.start,
                            end: range.end,
                            confidence: range.confidence,
                        })
                        .collect(),
                })
                .collect(),
            next: next_of(page.mentions.next.as_ref()),
        };
        Ok(Self {
            framing_version: FRAMING_VERSION,
            framing: FRAMING_STATEMENT,
            derived: true,
            as_of: page.as_of.get(),
            header: HeaderOut {
                entity: EntityOut::of(page.header.entity.clone()),
                merged: page.header.merged.iter().map(ToString::to_string).collect(),
            },
            facts,
            contradictions,
            mentions,
        })
    }
}

/// `GET /entities/{id}`: the entity page of 056, computed on this read.
///
/// An id absent from the caller's scope is the same 404 as an id that exists
/// in another scope (020 D-4).
pub async fn page(
    State(api): State<Api>,
    caller: Caller,
    Path(id): Path<String>,
    request: Result<Query<PageQuery>, QueryRejection>,
) -> ApiResult<Json<EntityPageResponse>> {
    let request = query(request)?;
    let scope = scope_ref(
        &request.scope,
        &request.project,
        &request.share,
        &request.owner,
    )
    .resolve(caller.sub())?;
    let defaults = PageBounds::default();
    let bounds = PageBounds {
        facts_per_predicate: request.facts.unwrap_or(defaults.facts_per_predicate),
        contradictions: request.contradictions.unwrap_or(defaults.contradictions),
        mentions: request.mentions.unwrap_or(defaults.mentions),
        resume: request
            .after
            .as_deref()
            .map(str::parse::<Continuation>)
            .transpose()?,
    };
    let as_of = request.as_of.map_or_else(now, UnixSeconds::new);
    let scope_id = ScopeId::of(&scope);
    let page = aicortex_graph::entity_page(
        api.app.store(),
        scope_id.as_str(),
        &EntityId::new(id),
        as_of,
        &bounds,
    )
    .await?
    .ok_or_else(ApiError::not_found)?;
    Ok(Json(EntityPageResponse::of(&page)?))
}
