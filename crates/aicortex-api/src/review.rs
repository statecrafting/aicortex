//! The review routes: the queue, its decisions, and promotion (spec 023 B-2,
//! B-5, B-6, B-8, B-9).
//!
//! Every route here is mounted under a prefix the chassis declares for a
//! person's token only (`router::bearer_routes`), and every handler refuses a
//! client acting as itself again before it does anything (B-8). The decisions
//! take the caller's subject and never a token, so the curate crate cannot be
//! handed an automated credential. What a reviewer is shown is a memory body,
//! so every body leaves inside 019's envelope (B-9); the queue's own fields
//! are ids, labels, and times.
//!
//! A reviewer's reason text is recorded in the ledger and on the item, and is
//! not returned by the listing: it is free text a person typed, and the
//! listing is what a client puts in front of a model (019 B-6).

use aicortex_curate::{
    ContradictionPair, DEFAULT_PAGE_ROWS, MAX_PAGE_ROWS, Promoter, Resolution, ReviewFilter,
    ReviewItem, ReviewKind, ReviewQueue, Reviewer,
};
use aicortex_recall::{EnvelopeInput, FramedMemories, frame_memories};
use aicortex_store::MemoryRepo;
use aicortex_types::{Memory, MemoryId, Status, TrustClass};
use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use rahi_types::UnixSeconds;
use serde::{Deserialize, Serialize};

use crate::capture::now;
use crate::dto::{ScopeQuery, ScopeRef};
use crate::error::{ApiError, ApiResult};
use crate::events::{ApiEvent, EventData, scope_label};
use crate::router::{Api, Caller};

/// The most rows one review page returns over HTTP.
///
/// Below the curate crate's own ceiling, because each row can bring up to
/// [`EVIDENCE_PER_ITEM`] memories with it.
pub const MAX_HTTP_PAGE_ROWS: u32 = 50;

/// How many memories one item shows a reviewer.
pub const EVIDENCE_PER_ITEM: usize = 3;

/// The reason code of a refusal to treat an automated client as a person.
const INTERACTIVE_REQUIRED: &str = "interactive_principal_required";

/// 403 unless the caller is a person (B-8, FR-003).
fn require_person(caller: &Caller) -> ApiResult<()> {
    if caller.is_service() {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            INTERACTIVE_REQUIRED,
            "review and promotion require a person's token; a client credentials token never \
             reviews or promotes",
        ));
    }
    Ok(())
}

/// A memory id from the path, as a 404 when it is not one.
fn memory_id(text: &str) -> ApiResult<MemoryId> {
    MemoryId::parse(text).map_err(|_| ApiError::not_found())
}

fn query<T>(query: Result<Query<T>, QueryRejection>) -> ApiResult<T> {
    query
        .map(|Query(value)| value)
        .map_err(|rejection| ApiError::invalid(rejection.body_text()))
}

fn body<T: for<'de> Deserialize<'de>>(bytes: &Bytes, what: &str) -> ApiResult<T> {
    serde_json::from_slice(bytes)
        .map_err(|error| ApiError::invalid(format!("the {what} is malformed: {error}")))
}

/// `GET /review`: the scope, an optional filter, and a keyset page.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReviewQuery {
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
    /// Only items of this kind.
    #[serde(default)]
    pub kind: Option<String>,
    /// Only items in this state.
    #[serde(default)]
    pub state: Option<String>,
    /// The cursor the previous page returned.
    #[serde(default)]
    pub cursor: Option<String>,
    /// Rows per page.
    #[serde(default)]
    pub limit: Option<u32>,
}

impl ReviewQuery {
    fn scope_ref(&self) -> ScopeRef {
        ScopeQuery {
            scope: self.scope.clone(),
            project: self.project.clone(),
            share: self.share.clone(),
            owner: self.owner.clone(),
        }
        .scope_ref()
    }

    fn filter(&self) -> ApiResult<ReviewFilter> {
        Ok(ReviewFilter {
            kind: self
                .kind
                .as_deref()
                .map(str::parse)
                .transpose()
                .map_err(ApiError::from)?,
            state: self
                .state
                .as_deref()
                .map(str::parse)
                .transpose()
                .map_err(ApiError::from)?,
        })
    }
}

/// The two edges of a contradiction, by id.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ContradictionView {
    /// The lesser edge id.
    pub edge_a: String,
    /// The greater edge id.
    pub edge_b: String,
    /// The shared subject entity's id.
    pub subject: String,
    /// The shared predicate, from the closed vocabulary (017 D-1).
    pub predicate: String,
}

impl From<ContradictionPair> for ContradictionView {
    fn from(pair: ContradictionPair) -> Self {
        Self {
            edge_a: pair.edge_a,
            edge_b: pair.edge_b,
            subject: pair.subject,
            predicate: pair.predicate,
        }
    }
}

/// One item as the listing shows it: bare fields, and the ids of the
/// memories whose envelopes sit beside the list.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ReviewItemView {
    /// The item's id.
    pub id: String,
    /// `quarantine`, `contradiction`, `promotion_request`, or `gate_override`.
    pub kind: String,
    /// `open`, `resolved`, or `withdrawn`.
    pub state: String,
    /// The memory or contradiction it is about.
    pub subject: String,
    /// When it was opened.
    pub created: u64,
    /// Who asked, for a request a person made.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requested_by: Option<String>,
    /// Who decided.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_by: Option<String>,
    /// When it was decided.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_at: Option<u64>,
    /// What was decided.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution: Option<String>,
    /// The ledger Decision that records it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decision_id: Option<String>,
    /// The memories the reviewer decides on, whose envelopes are in this
    /// response.
    pub memories: Vec<MemoryId>,
    /// The two edges, for a contradiction.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contradiction: Option<ContradictionView>,
    /// The trust class this view is shown at: the lowest of the memories it
    /// summarizes, never higher (019 B-5, FR-005).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trust: Option<String>,
}

/// The answer to `GET /review`.
#[derive(Serialize)]
pub struct ReviewResponse {
    /// The framing statement and the envelopes of every memory shown, as 019
    /// serializes them.
    #[serde(flatten)]
    pub framed: FramedMemories,
    /// The items, oldest first.
    pub items: Vec<ReviewItemView>,
    /// The cursor of the next page, when there is one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
}

/// The answer to `GET /review/count`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ReviewCount {
    /// Items waiting for a person, of every kind.
    pub open: u64,
    /// The same, by kind.
    pub by_kind: std::collections::BTreeMap<String, u64>,
}

/// The answer to a resolution, a promotion, or a demotion: bare fields only.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DecisionResponse {
    /// The memory or item concerned.
    pub id: String,
    /// What was done.
    pub outcome: String,
    /// The ledger Decision that records it.
    pub decision_id: String,
    /// When the promotion is next reviewed, for a promotion.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review_due: Option<u64>,
}

/// The body of `POST /review/{id}/resolve` (B-2): a resolution and the
/// fields it takes.
///
/// A flat struct rather than a tagged enum so that an unknown field is
/// refused for every resolution, unit variants included; `serde` ignores
/// extra fields on a unit variant of an internally tagged enum. A field the
/// named resolution does not take is a 400 as well.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ResolveBody {
    /// `admit`, `reject`, `keep_edge`, `disjoint`, `approve`, `decline`, or
    /// `acknowledge`.
    pub resolution: String,
    /// `keep_edge`: the edge that stays active.
    #[serde(default)]
    pub edge: Option<String>,
    /// `keep_edge` and `disjoint`: why.
    #[serde(default)]
    pub reason: Option<String>,
    /// `disjoint`: the second the earlier period ends and the later begins.
    #[serde(default)]
    pub boundary: Option<u64>,
    /// `approve`: seconds until the promotion is reviewed again.
    #[serde(default)]
    pub review_interval_secs: Option<u64>,
}

impl ResolveBody {
    /// The typed resolution this names.
    ///
    /// # Errors
    ///
    /// 400 for a resolution outside the vocabulary, a field the resolution
    /// requires and the body lacks, or a field the resolution does not take.
    pub fn into_resolution(self) -> ApiResult<Resolution> {
        let Self {
            resolution,
            edge,
            reason,
            boundary,
            review_interval_secs,
        } = self;
        let present = [
            ("edge", edge.is_some()),
            ("reason", reason.is_some()),
            ("boundary", boundary.is_some()),
            ("review_interval_secs", review_interval_secs.is_some()),
        ];
        let takes: &[&str] = match resolution.as_str() {
            "admit" | "reject" | "decline" | "acknowledge" => &[],
            "keep_edge" => &["edge", "reason"],
            "disjoint" => &["boundary", "reason"],
            "approve" => &["review_interval_secs"],
            other => {
                return Err(ApiError::invalid(format!(
                    "resolution must be one of admit, reject, keep_edge, disjoint, approve, \
                     decline, acknowledge; got {other:?}"
                )));
            }
        };
        if let Some((field, _)) = present
            .iter()
            .find(|(field, given)| *given && !takes.contains(field))
        {
            return Err(ApiError::invalid(format!(
                "a {resolution} resolution takes no {field}"
            )));
        }
        let need = |field: &str, value: Option<String>| {
            value.ok_or_else(|| {
                ApiError::invalid(format!("a {resolution} resolution needs a {field}"))
            })
        };
        Ok(match resolution.as_str() {
            "admit" => Resolution::Admit,
            "reject" => Resolution::Reject,
            "decline" => Resolution::Decline,
            "acknowledge" => Resolution::Acknowledge,
            "keep_edge" => Resolution::KeepEdge {
                survivor: need("edge", edge)?,
                reason: need("reason", reason)?,
            },
            "disjoint" => {
                Resolution::Disjoint {
                    boundary: UnixSeconds::new(boundary.ok_or_else(|| {
                        ApiError::invalid("a disjoint resolution needs a boundary")
                    })?),
                    reason: need("reason", reason)?,
                }
            }
            _ => Resolution::Approve {
                interval_secs: review_interval_secs,
            },
        })
    }
}

/// The body of `POST /review`: ask for a promotion.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RequestBody {
    /// Only `promotion_request` is asked for over HTTP: the system opens the
    /// other kinds itself.
    pub kind: String,
    /// The memory to put forward.
    pub memory: String,
    /// The scope; the caller's personal scope when absent.
    #[serde(default)]
    pub scope: Option<ScopeRef>,
}

/// The optional body of `POST /promote/{id}`.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PromoteBody {
    /// Seconds until the promotion is reviewed again.
    #[serde(default)]
    pub review_interval_secs: Option<u64>,
}

fn publish(api: &Api, scope: &aicortex_types::Scope, id: &str, state: &str) {
    api.events.publish(ApiEvent {
        owner: scope.owner.clone(),
        name: "review",
        data: EventData {
            scope: scope_label(scope),
            id: Some(id.to_owned()),
            state: state.to_owned(),
            reason_code: None,
        },
    });
}

/// The memory `id` in `scope` when it can be shown: present and not erased.
async fn showable(
    api: &Api,
    scope: &aicortex_types::Scope,
    id: MemoryId,
) -> ApiResult<Option<Memory>> {
    let found = MemoryRepo::new().get(api.app.store(), scope, id).await?;
    Ok(found.filter(|memory| memory.status != Status::Erased))
}

/// Build the response for `items`, enveloping the memories they decide on.
async fn present(
    api: &Api,
    scope: &aicortex_types::Scope,
    items: Vec<ReviewItem>,
    next: Option<String>,
) -> ApiResult<ReviewResponse> {
    let queue = ReviewQueue::new();
    let mut shown: Vec<Memory> = Vec::new();
    let mut views = Vec::with_capacity(items.len());
    for item in items {
        let evidence = queue.evidence(api.app.store(), scope, &item).await?;
        let contradiction = if item.kind == ReviewKind::Contradiction {
            queue
                .contradiction(api.app.store(), scope, &item.subject)
                .await?
                .map(ContradictionView::from)
        } else {
            None
        };
        let mut ids = Vec::new();
        let mut trust: Option<TrustClass> = None;
        for id in evidence.into_iter().take(EVIDENCE_PER_ITEM) {
            let Some(memory) = showable(api, scope, id).await? else {
                continue;
            };
            ids.push(id);
            trust = Some(match trust {
                Some(lowest) => lowest.min(memory.trust.clone()),
                None => memory.trust.clone(),
            });
            if !shown.iter().any(|seen| seen.id == id) {
                shown.push(memory);
            }
        }
        views.push(ReviewItemView {
            id: item.id,
            kind: item.kind.label().to_owned(),
            state: item.state.label().to_owned(),
            subject: item.subject,
            created: item.created.get(),
            requested_by: item.requested_by.map(|sub| sub.as_str().to_owned()),
            resolved_by: item.resolved_by.map(|sub| sub.as_str().to_owned()),
            resolved_at: item.resolved_at.map(UnixSeconds::get),
            resolution: item.resolution,
            decision_id: item.decision_id,
            memories: ids,
            contradiction,
            trust: trust.map(|class| class.label().to_owned()),
        });
    }
    let ids: Vec<MemoryId> = shown.iter().map(|memory| memory.id).collect();
    let erased = MemoryRepo::new()
        .origin_erased(api.app.store(), scope, &ids)
        .await?;
    let framed = frame_memories(
        shown
            .iter()
            .map(|memory| EnvelopeInput::new(memory, erased.contains(&memory.id))),
    )
    .map_err(|_| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            "a memory could not be enveloped",
        )
    })?;
    Ok(ReviewResponse {
        framed,
        items: views,
        next,
    })
}

/// `GET /review`: the queue, oldest first (B-2).
pub async fn list(
    State(api): State<Api>,
    caller: Caller,
    request: Result<Query<ReviewQuery>, QueryRejection>,
) -> ApiResult<Json<ReviewResponse>> {
    require_person(&caller)?;
    let request = query(request)?;
    let scope = request.scope_ref().resolve(caller.sub())?;
    let filter = request.filter()?;
    let limit = request.limit.unwrap_or(DEFAULT_PAGE_ROWS);
    if limit == 0 || limit > MAX_PAGE_ROWS.min(MAX_HTTP_PAGE_ROWS) {
        return Err(ApiError::invalid(format!(
            "limit must be 1 to {}; got {limit}",
            MAX_PAGE_ROWS.min(MAX_HTTP_PAGE_ROWS)
        )));
    }
    let store = api.app.store();
    let queue = ReviewQueue::new();
    queue.collect(store, &scope, now()).await?;
    let page = queue
        .list(
            store,
            &scope,
            filter,
            Some(limit),
            request.cursor.as_deref(),
        )
        .await?;
    Ok(Json(present(&api, &scope, page.items, page.next).await?))
}

/// `GET /review/count`: how many items wait, for the MCP resource of 021 B-8.
pub async fn count(
    State(api): State<Api>,
    caller: Caller,
    request: Result<Query<ScopeQuery>, QueryRejection>,
) -> ApiResult<Json<ReviewCount>> {
    require_person(&caller)?;
    let scope = query(request)?.scope_ref().resolve(caller.sub())?;
    let store = api.app.store();
    let queue = ReviewQueue::new();
    queue.collect(store, &scope, now()).await?;
    let mut by_kind = std::collections::BTreeMap::new();
    let mut open = 0_u64;
    for kind in ReviewKind::ALL {
        let waiting = queue.open_count(store, &scope, Some(kind)).await?;
        open = open.saturating_add(waiting);
        by_kind.insert(kind.label().to_owned(), waiting);
    }
    Ok(Json(ReviewCount { open, by_kind }))
}

/// `POST /review`: put a memory forward for promotion (B-1).
///
/// Asking decides nothing, and it needs `memory.write` and a person's token;
/// deciding needs `memory.promote` (B-8).
pub async fn request(
    State(api): State<Api>,
    caller: Caller,
    bytes: Bytes,
) -> ApiResult<(StatusCode, Json<ReviewResponse>)> {
    require_person(&caller)?;
    let request: RequestBody = body(&bytes, "request")?;
    if request.kind != ReviewKind::PromotionRequest.label() {
        return Err(ApiError::invalid(format!(
            "only a promotion_request can be opened over HTTP; got {:?}",
            request.kind
        )));
    }
    let scope = request
        .scope
        .unwrap_or_else(ScopeRef::personal)
        .resolve(caller.sub())?;
    let memory = memory_id(&request.memory)?;
    let item = ReviewQueue::new()
        .request_promotion(api.app.store(), &scope, memory, caller.sub(), now())
        .await?;
    publish(&api, &scope, &item.id, "open");
    Ok((
        StatusCode::CREATED,
        Json(present(&api, &scope, vec![item], None).await?),
    ))
}

/// `POST /review/{id}/resolve`: decide an item (B-2, B-3, B-4).
pub async fn resolve(
    State(api): State<Api>,
    caller: Caller,
    Path(id): Path<String>,
    request: Result<Query<ScopeQuery>, QueryRejection>,
    bytes: Bytes,
) -> ApiResult<Json<DecisionResponse>> {
    require_person(&caller)?;
    let scope = query(request)?.scope_ref().resolve(caller.sub())?;
    let resolution = body::<ResolveBody>(&bytes, "resolution")?.into_resolution()?;
    let item = Reviewer::new(api.app.store(), api.app.ledger())
        .resolve(&scope, &id, caller.sub(), &resolution, now())
        .await?;
    publish(&api, &scope, &item.id, "resolved");
    let decision_id = item.decision_id.ok_or_else(|| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            "a resolved item names no decision",
        )
    })?;
    Ok(Json(DecisionResponse {
        id: item.id,
        outcome: resolution.label().to_owned(),
        decision_id,
        review_due: None,
    }))
}

/// `POST /promote/{id}`: the human boundary of 019 B-4 (B-5, FR-002, FR-003).
pub async fn promote(
    State(api): State<Api>,
    caller: Caller,
    Path(id): Path<String>,
    request: Result<Query<ScopeQuery>, QueryRejection>,
    bytes: Bytes,
) -> ApiResult<Json<DecisionResponse>> {
    require_person(&caller)?;
    let scope = query(request)?.scope_ref().resolve(caller.sub())?;
    let memory = memory_id(&id)?;
    let options: PromoteBody = if bytes.is_empty() {
        PromoteBody::default()
    } else {
        body(&bytes, "promotion")?
    };
    let promoted = Promoter::new(api.app.store(), api.app.ledger())
        .promote(
            &scope,
            memory,
            caller.sub(),
            options.review_interval_secs,
            now(),
        )
        .await?;
    publish(&api, &scope, &id, "promoted");
    Ok(Json(DecisionResponse {
        id,
        outcome: "promoted".to_owned(),
        decision_id: promoted.decision,
        review_due: Some(promoted.review_due.get()),
    }))
}

/// `POST /demote/{id}`: back to assertion grade (B-6).
pub async fn demote(
    State(api): State<Api>,
    caller: Caller,
    Path(id): Path<String>,
    request: Result<Query<ScopeQuery>, QueryRejection>,
) -> ApiResult<Json<DecisionResponse>> {
    require_person(&caller)?;
    let scope = query(request)?.scope_ref().resolve(caller.sub())?;
    let memory = memory_id(&id)?;
    let demoted = Promoter::new(api.app.store(), api.app.ledger())
        .demote(&scope, memory, caller.sub(), now())
        .await?;
    publish(&api, &scope, &id, "demoted");
    Ok(Json(DecisionResponse {
        id,
        outcome: "demoted".to_owned(),
        decision_id: demoted.decision,
        review_due: None,
    }))
}
