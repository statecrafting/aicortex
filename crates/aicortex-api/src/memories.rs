//! The memory routes: list, read, correct or supersede, erase, count, and
//! promote (B-4, B-5, D-3).
//!
//! Every route resolves the scope against the caller first (B-2): a scope
//! the caller cannot reach is a 404 decided before any store read. Every
//! body returned goes through [`aicortex_recall::frame_memories`], the one
//! outbound serializer of 019, with the `origin_erased` marker read beside
//! the memories it describes.

use aicortex_gate::{Candidate, Origin, Verdict};
use aicortex_recall::{EnvelopeInput, frame_memories};
use aicortex_store::{
    Authority, Counters, DEFAULT_PAGE_ROWS, Eraser, Erasure, Lifecycle, MemoryFilter, MemoryRepo,
};
use aicortex_types::{Memory, MemoryId, MemoryKind, Scope};
use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use rahi_store::TxnBuilder;

use crate::capture::{commit, now, parts};
use crate::dto::{
    CandidateIn, EraseQuery, EraseResponse, ListQuery, MemoriesResponse, MemoryMeta, PatchRequest,
    PatchResponse, ScopeQuery, ScopeRef, StatsResponse,
};
use crate::error::{ApiError, ApiResult};
use crate::events::{ApiEvent, EventData, scope_label};
use crate::router::{Api, Caller};

/// The query string, or a problem document saying why it is not one.
fn query<T>(query: Result<Query<T>, QueryRejection>) -> ApiResult<T> {
    query
        .map(|Query(value)| value)
        .map_err(|rejection| ApiError::invalid(rejection.body_text()))
}

/// A memory id from the path.
fn memory_id(text: &str) -> ApiResult<MemoryId> {
    MemoryId::parse(text).map_err(|_| ApiError::not_found())
}

/// The memory `id` in `scope`, or 404.
async fn existing(api: &Api, scope: &Scope, id: MemoryId) -> ApiResult<Memory> {
    MemoryRepo::new()
        .get(api.app.store(), scope, id)
        .await?
        .ok_or_else(ApiError::not_found)
}

/// Envelope `memories`, with their markers, beside their bare fields (B-5).
async fn framed(
    api: &Api,
    scope: &Scope,
    memories: &[Memory],
    next: Option<String>,
) -> ApiResult<MemoriesResponse> {
    let ids: Vec<MemoryId> = memories.iter().map(|memory| memory.id).collect();
    let erased = MemoryRepo::new()
        .origin_erased(api.app.store(), scope, &ids)
        .await?;
    let framed = frame_memories(
        memories
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
    Ok(MemoriesResponse {
        framed,
        items: memories.iter().map(MemoryMeta::of).collect(),
        next,
    })
}

/// Tell the caller's open streams that a memory changed state (B-6).
fn changed(api: &Api, scope: &Scope, id: MemoryId, state: &str) {
    api.events.publish(ApiEvent {
        owner: scope.owner.clone(),
        name: "memory",
        data: EventData {
            scope: scope_label(scope),
            id: Some(id.to_string()),
            state: state.to_owned(),
            reason_code: None,
        },
    });
}

/// `GET /memories`: one keyset page, newest first (012 B-7).
pub async fn list(
    State(api): State<Api>,
    caller: Caller,
    request: Result<Query<ListQuery>, QueryRejection>,
) -> ApiResult<Json<MemoriesResponse>> {
    let request = query(request)?;
    let scope = request.scope_ref().resolve(caller.sub())?;
    let filter = MemoryFilter {
        status: request.status_filter()?,
        kind: request.kind_filter()?,
    };
    let key = api.cursor_key().await?;
    let listing = MemoryRepo::new()
        .list(
            api.app.store(),
            key,
            &scope,
            &filter,
            request.limit.unwrap_or(DEFAULT_PAGE_ROWS),
            request.cursor.as_deref(),
        )
        .await?;
    Ok(Json(
        framed(&api, &scope, &listing.memories, listing.next).await?,
    ))
}

/// `GET /memories/{id}`.
pub async fn get_one(
    State(api): State<Api>,
    caller: Caller,
    Path(id): Path<String>,
    request: Result<Query<ScopeQuery>, QueryRejection>,
) -> ApiResult<Json<MemoriesResponse>> {
    let scope = query(request)?.scope_ref().resolve(caller.sub())?;
    let memory = existing(&api, &scope, memory_id(&id)?).await?;
    Ok(Json(framed(&api, &scope, &[memory], None).await?))
}

/// `PATCH /memories/{id}`: correct it, or record what superseded it (014 B-3,
/// B-4).
pub async fn patch_one(
    State(api): State<Api>,
    caller: Caller,
    Path(id): Path<String>,
    body: Bytes,
) -> ApiResult<Json<PatchResponse>> {
    let request: PatchRequest = serde_json::from_slice(&body)
        .map_err(|error| ApiError::invalid(format!("the patch is malformed: {error}")))?;
    let scope = request
        .scope
        .clone()
        .unwrap_or_else(ScopeRef::personal)
        .resolve(caller.sub())?;
    let old = existing(&api, &scope, memory_id(&id)?).await?.id;

    let response = match (&request.correction, request.superseded_by) {
        (Some(correction), None) => {
            let candidate = CandidateIn {
                kind: MemoryKind::Correction,
                text: correction.text.clone(),
                title: correction.title.clone(),
                source: None,
                captured_at: None,
            };
            let mut parts = parts(&caller, &scope, &candidate, now())?;
            parts.provenance.derived_from = vec![old];
            let candidate = Candidate::new(parts, Origin::Authenticated);
            let verdict = api.gate.evaluate(&candidate);
            let (stored, _) = commit(&api, &caller, &candidate, &verdict, Some(old)).await?;
            match (verdict, stored) {
                (Verdict::Refuse(reason), _) => return Err(ApiError::refused(&reason)),
                (_, Some(captured)) => PatchResponse {
                    id: old,
                    superseded_by: captured.id(),
                    merged: captured.is_merge(),
                },
                (_, None) => {
                    return Err(ApiError::new(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "internal",
                        "an admitted correction stored no row",
                    ));
                }
            }
        }
        (None, Some(new)) => {
            let new = existing(&api, &scope, new).await?.id;
            let mut txn = TxnBuilder::new();
            Lifecycle::new()
                .supersede(api.app.store(), &mut txn, &scope, old, new, now())
                .await?;
            api.app.store().txn(txn.into_statements()).await?;
            PatchResponse {
                id: old,
                superseded_by: new,
                merged: false,
            }
        }
        _ => {
            return Err(ApiError::invalid(
                "a patch names exactly one of correction and superseded_by",
            ));
        }
    };
    changed(&api, &scope, old, "superseded");
    Ok(Json(response))
}

/// `DELETE /memories/{id}`: erasure (014 B-7).
pub async fn erase(
    State(api): State<Api>,
    caller: Caller,
    Path(id): Path<String>,
    request: Result<Query<EraseQuery>, QueryRejection>,
) -> ApiResult<Json<EraseResponse>> {
    let request = query(request)?;
    let scope = request.scope_ref().resolve(caller.sub())?;
    let id = existing(&api, &scope, memory_id(&id)?).await?.id;
    let authority = Authority::of(caller.sub().clone());
    let mut erasure = Erasure::new(&scope, id, &authority, now());
    if request.cascade {
        erasure = erasure.cascading();
    }
    let erased = Eraser::new()
        .erase(api.app.store(), api.app.ledger(), &erasure)
        .await?;
    changed(&api, &scope, id, "erased");
    Ok(Json(EraseResponse::from(erased)))
}

/// `GET /stats`: the scope's counters, one bounded read (012 B-6).
pub async fn stats(
    State(api): State<Api>,
    caller: Caller,
    request: Result<Query<ScopeQuery>, QueryRejection>,
) -> ApiResult<Json<StatsResponse>> {
    let scope = query(request)?.scope_ref().resolve(caller.sub())?;
    let stats = Counters::stats(api.app.store(), &scope).await?;
    Ok(Json(StatsResponse::from(stats)))
}

/// `POST /promote/{id}`: the human boundary of 019 B-4 (D-12).
///
/// The route and its authorization are this spec's: `memory.promote`, a
/// person's token (the chassis refuses a client credentials token on this
/// subtree before the handler runs, and the handler refuses one again), and
/// a memory in a scope the caller reaches. The act itself, appending the
/// Decision and writing the trust class, is 023 B-5's, and until 023 lands
/// this answers 501 without writing anything.
pub async fn promote(
    State(api): State<Api>,
    caller: Caller,
    Path(id): Path<String>,
    request: Result<Query<ScopeQuery>, QueryRejection>,
) -> ApiResult<Json<()>> {
    if caller.is_service() {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "interactive_principal_required",
            "promotion requires a person's token; a client credentials token never promotes",
        ));
    }
    let scope = query(request)?.scope_ref().resolve(caller.sub())?;
    existing(&api, &scope, memory_id(&id)?).await?;
    Err(ApiError::new(
        StatusCode::NOT_IMPLEMENTED,
        "promotion_unavailable",
        "promotion is written by the review spec (023), which has not landed",
    ))
}
