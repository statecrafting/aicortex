//! `GET /entities` and `GET /entities/{id}/neighbors` (B-4, 020 D-3, 017 D-9).
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

use std::collections::BTreeSet;

use aicortex_graph::{
    DEFAULT_DEPTH, DEFAULT_FANOUT, Entity, EntityId, EntityKind, Hop, HopEnd, MAX_LIST, Predicate,
    WalkFilter, WalkHit, WalkOptions,
};
use aicortex_recall::FRAMING_STATEMENT;
use aicortex_store::ScopeId;
use axum::Json;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Path, Query, State};
use serde::{Deserialize, Serialize};

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
