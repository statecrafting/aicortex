//! `POST /capture` (B-4, B-7, B-8, B-9).
//!
//! One candidate at a time through the write gate and into the store, each in
//! its own transaction (constitution XI): the row, its provenance, its
//! counters, its outbox work and embedding job, and for a quarantine the
//! Decision's digest key all commit together, and a refusal stores nothing
//! but the key. A refusal and a quarantine are then ledgered (013 B-9). A
//! batch is a sequence of independent captures, so one refused candidate
//! does not undo its neighbours.
//!
//! A request carrying a single candidate that the gate refuses answers 422
//! with the reason code (B-7); a batch answers 200 with an outcome per
//! candidate, a refused one carrying its reason code and problem `type`
//! (D-9). Neither ever echoes refused content.

use aicortex_gate::{
    Candidate, DigestRef, KIND_QUARANTINE, KIND_REFUSE, LedgerEntry, Origin, Reason, Verdict,
    ledger_entry,
};
use aicortex_store::{Captured, DIGEST_ALGORITHM, DecisionKey, DecisionKeyRepo, Lifecycle};
use aicortex_types::{
    Actor, ActorId, AgentOrigin, Importance, MemoryBody, MemoryId, MemoryParts, Provenance, Scope,
    SourceRef, SourceSystem, TrustClass,
};
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use rahi_ledger::{Decision, DecisionId, DecisionKind, Outcome};
use rahi_store::{Envelope, TxnBuilder};
use rahi_types::{Error, Revision, UnixSeconds};
use serde::{Deserialize, Serialize};

use crate::dto::{CandidateIn, CaptureOutcome, CaptureRequest, CaptureResponse, ScopeRef};
use crate::error::{ApiError, ApiResult, problem_type};
use crate::events::{ApiEvent, EventData, scope_label};
use crate::router::{Api, Caller};

/// The request header a retrying client names its attempt with (B-8).
pub const IDEMPOTENCY_KEY: &str = "idempotency-key";

/// The response header that marks a replayed answer.
pub const IDEMPOTENT_REPLAYED: &str = "idempotent-replayed";

/// The longest idempotency key accepted.
pub const MAX_IDEMPOTENCY_KEY_LEN: usize = 255;

/// The source system a candidate is attributed to when neither it nor the
/// presenting client names one that is well formed.
pub const DEFAULT_SOURCE_SYSTEM: &str = "aicortex-api";

/// The lowercase hex of `bytes`.
#[must_use]
pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        })
}

/// The lowercase hex SHA-256 of `parts`, each length-prefixed so no two
/// sequences share a digest.
fn digest(parts: &[&[u8]]) -> String {
    let mut context = ring::digest::Context::new(&ring::digest::SHA256);
    for part in parts {
        context.update(&(part.len() as u64).to_be_bytes());
        context.update(part);
    }
    hex(context.finish().as_ref())
}

/// The current time, in Unix seconds.
fn now() -> UnixSeconds {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    UnixSeconds::new(secs)
}

/// What an idempotency key remembers: the request it answered and the answer.
#[derive(Debug, Deserialize, Serialize)]
struct Remembered {
    request: String,
    status: u16,
    body: String,
}

/// `POST /capture`.
pub async fn capture(
    State(api): State<Api>,
    caller: Caller,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let key = match idempotency_key(&headers) {
        Ok(key) => key,
        Err(error) => return error.into_response(),
    };
    let Some(key) = key else {
        return answer(&api, &caller, &body)
            .await
            .map(axum::Json)
            .into_response();
    };

    // The key is the caller's, so two callers cannot collide on one, and it
    // is bound to the request it first answered, so it cannot be reused for
    // different content (B-8).
    let slot = format!(
        "aicortex.api.idempotency.{}",
        digest(&[
            caller.sub().as_str().as_bytes(),
            caller.client_id().unwrap_or_default().as_bytes(),
            key.as_bytes(),
        ])
    );
    let request = digest(&[&body]);
    let store = api.app.store();
    match store.kv_get::<Remembered>(&slot).await {
        Ok(Some(remembered)) if remembered.request == request => {
            return replay(&remembered);
        }
        Ok(Some(_)) => {
            return ApiError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "idempotency_key_reused",
                "this Idempotency-Key already answered a different request",
            )
            .into_response();
        }
        Ok(None) => {}
        Err(error) => return ApiError::from(error).into_response(),
    }

    let (status, json) = match answer(&api, &caller, &body).await {
        Ok(response) => (
            StatusCode::OK,
            serde_json::to_string(&response).unwrap_or_default(),
        ),
        Err(error) => (
            error.status(),
            serde_json::to_string(&error.problem()).unwrap_or_default(),
        ),
    };
    // A failure of this cell is not remembered, so a retry tries again; a
    // refusal is, because the same request would be refused again.
    if !status.is_server_error() {
        let remembered = Remembered {
            request,
            status: status.as_u16(),
            body: json.clone(),
        };
        // A lost memo costs only the replay: the retry recaptures, and the
        // fingerprint merge of 014 B-2 keeps it from creating a second row.
        let _ = store
            .kv_put(&slot, &remembered, Some(api.limits.idempotency_ttl_secs))
            .await;
    }
    respond(status, json, false)
}

/// The key a request names, validated.
fn idempotency_key(headers: &HeaderMap) -> ApiResult<Option<String>> {
    let Some(value) = headers.get(IDEMPOTENCY_KEY) else {
        return Ok(None);
    };
    let key = value
        .to_str()
        .ok()
        .filter(|key| {
            !key.is_empty()
                && key.len() <= MAX_IDEMPOTENCY_KEY_LEN
                && key.bytes().all(|byte| byte.is_ascii_graphic())
        })
        .ok_or_else(|| {
            ApiError::invalid(format!(
                "Idempotency-Key is 1 to {MAX_IDEMPOTENCY_KEY_LEN} visible ASCII characters"
            ))
        })?;
    Ok(Some(key.to_owned()))
}

/// The remembered answer, marked as a replay.
fn replay(remembered: &Remembered) -> Response {
    let status = StatusCode::from_u16(remembered.status).unwrap_or(StatusCode::OK);
    respond(status, remembered.body.clone(), true)
}

fn respond(status: StatusCode, json: String, replayed: bool) -> Response {
    let content_type = if status.is_success() {
        "application/json"
    } else {
        crate::error::PROBLEM_JSON
    };
    let mut response = (status, json).into_response();
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    if replayed {
        headers.insert(IDEMPOTENT_REPLAYED, HeaderValue::from_static("true"));
    }
    response
}

/// Parse, bound, and capture every candidate.
async fn answer(api: &Api, caller: &Caller, body: &[u8]) -> ApiResult<CaptureResponse> {
    let request: CaptureRequest = serde_json::from_slice(body)
        .map_err(|error| ApiError::invalid(format!("the capture request is malformed: {error}")))?;
    let count = request.candidates.len();
    if count == 0 || count > api.limits.max_batch {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "batch_bounds",
            format!(
                "a capture carries 1 to {} candidates, not {count}",
                api.limits.max_batch
            ),
        ));
    }
    let scope = request
        .scope
        .unwrap_or_else(ScopeRef::personal)
        .resolve(caller.sub())?;

    let mut results = Vec::with_capacity(count);
    for (index, candidate) in request.candidates.iter().enumerate() {
        let (outcome, refused) = capture_one(api, caller, &scope, candidate, index).await?;
        if count == 1
            && let Some(reason) = refused
        {
            return Err(ApiError::refused(&reason));
        }
        results.push(outcome);
    }
    Ok(CaptureResponse { results })
}

/// Who the record says made the claim (011 B-7).
///
/// A person's token makes a human actor named by the subject and labelled
/// with the client that presented it; a client acting as itself is an agent.
/// The trust class is [`TrustClass::Assertion`] either way: nothing captured
/// through this surface is instruction grade (019 B-4).
fn actor(caller: &Caller) -> ApiResult<Actor> {
    let invalid = |error: aicortex_types::TypeError| ApiError::invalid(error.to_string());
    if caller.is_service() {
        let client = caller.client_id().unwrap_or_default().to_owned();
        let origin = AgentOrigin {
            client: Some(client.clone()),
            model: None,
        };
        return Ok(Actor::agent(ActorId::new(client).map_err(invalid)?, origin));
    }
    let human = Actor::human(ActorId::new(caller.sub().as_str()).map_err(invalid)?);
    Ok(match caller.client_id() {
        Some(client) => human.labelled(client),
        None => human,
    })
}

/// The source a candidate is attributed to.
fn source(caller: &Caller, candidate: &CandidateIn) -> ApiResult<SourceRef> {
    let named = candidate.source.as_ref();
    let system = match named.and_then(|source| source.system.clone()) {
        Some(system) => {
            SourceSystem::new(system).map_err(|error| ApiError::invalid(error.to_string()))?
        }
        None => caller
            .client_id()
            .and_then(|client| SourceSystem::new(client.to_ascii_lowercase()).ok())
            .map_or_else(|| SourceSystem::new(DEFAULT_SOURCE_SYSTEM), Ok)
            .map_err(|error| ApiError::invalid(error.to_string()))?,
    };
    let mut source = SourceRef::new(system);
    if let Some(external) = named.and_then(|source| source.external_id.clone()) {
        source = source.with_external_id(external);
    }
    if let Some(locator) = named.and_then(|source| source.locator.clone()) {
        source = source.with_locator(locator);
    }
    Ok(source)
}

/// The record a candidate becomes, before the gate judges it.
fn parts(
    caller: &Caller,
    scope: &Scope,
    candidate: &CandidateIn,
    at: UnixSeconds,
) -> ApiResult<MemoryParts> {
    let mut body = MemoryBody::text(candidate.text.clone());
    if let Some(title) = &candidate.title {
        body = body.with_title(title.clone());
    }
    let captured_at = candidate.captured_at.map_or(at, UnixSeconds::new);
    Ok(MemoryParts {
        id: MemoryId::now_v7(),
        scope: scope.clone(),
        kind: candidate.kind,
        body,
        actor: actor(caller)?,
        provenance: Provenance::captured(source(caller, candidate)?, captured_at, at),
        trust: TrustClass::Assertion,
        importance: Importance::at(at).map_err(|error| ApiError::invalid(error.to_string()))?,
        created: at,
    })
}

/// The outbox work a capture stages (012 B-3).
fn work(scope: &Scope, id: MemoryId) -> Envelope {
    Envelope::new(
        "memory",
        Some(scope.owner.as_str().to_owned()),
        id.to_string(),
        Revision::new(1),
    )
}

/// Judge one candidate and do exactly what the verdict says.
async fn capture_one(
    api: &Api,
    caller: &Caller,
    scope: &Scope,
    candidate: &CandidateIn,
    index: usize,
) -> ApiResult<(CaptureOutcome, Option<Reason>)> {
    let parts = parts(caller, scope, candidate, now())?;
    let candidate = Candidate::new(parts, Origin::Authenticated);
    let verdict = api.gate.evaluate(&candidate);
    let (stored, decision) = commit(api, caller, &candidate, &verdict).await?;

    let (label, problem) = match &verdict {
        Verdict::Admit(_) => ("admit", None),
        Verdict::Quarantine(..) => ("quarantine", None),
        Verdict::Refuse(reason) => ("refuse", Some(problem_type(reason.code()))),
    };
    let reason_code = verdict.reason().map(|reason| reason.code().to_owned());
    api.events.publish(ApiEvent {
        owner: scope.owner.clone(),
        name: "capture",
        data: EventData {
            scope: scope_label(scope),
            id: stored.map(|captured| captured.id().to_string()),
            state: label.to_owned(),
            reason_code: reason_code.clone(),
        },
    });
    let outcome = CaptureOutcome {
        index,
        verdict: label.to_owned(),
        id: stored.map(Captured::id),
        merged: stored.is_some_and(Captured::is_merge),
        reason_code,
        problem,
        decision,
    };
    let refused = match verdict {
        Verdict::Refuse(reason) => Some(reason),
        Verdict::Admit(_) | Verdict::Quarantine(..) => None,
    };
    Ok((outcome, refused))
}

/// The one transaction a verdict implies, then its Decision (013 B-9).
async fn commit(
    api: &Api,
    caller: &Caller,
    candidate: &Candidate,
    verdict: &Verdict,
) -> ApiResult<(Option<Captured>, Option<String>)> {
    let store = api.app.store();
    let mut txn = TxnBuilder::new();
    let captured = match verdict.admitted() {
        Some(admitted) => {
            let memory = admitted.memory();
            let work = work(&memory.scope, memory.id);
            Some(
                Lifecycle::new()
                    .capture(store, &mut txn, admitted, &work)
                    .await?,
            )
        }
        None => None,
    };

    let Some(reason) = verdict.reason() else {
        store.txn(txn.into_statements()).await?;
        return Ok((captured, None));
    };

    let key = DecisionKey::mint()?;
    let digest = DigestRef {
        digest: key.digest(&api.gate.digest_material(candidate)),
        algorithm: DIGEST_ALGORITHM.to_owned(),
        key_id: key.id().to_string(),
    };
    let stored = captured.map(Captured::id);
    let scope = &candidate.parts.scope;
    DecisionKeyRepo::stage(&mut txn, scope, &key, stored, candidate.parts.created);
    store.txn(txn.into_statements()).await?;

    let kind = if matches!(verdict, Verdict::Refuse(_)) {
        KIND_REFUSE
    } else {
        KIND_QUARANTINE
    };
    let entry = ledger_entry(kind, reason, scope, caller.sub(), &digest, stored);
    let id = append(api, &entry, candidate.parts.id).await?;
    Ok((captured, Some(id)))
}

/// Append one gate Decision, with an id derived from the candidate so a
/// replay is visible rather than silent.
async fn append(api: &Api, entry: &LedgerEntry, candidate: MemoryId) -> Result<String, Error> {
    let id = format!("{}-{candidate}", entry.kind);
    let outcome = if entry.denied {
        Outcome::Deny
    } else {
        Outcome::Allow
    };
    api.app
        .ledger()
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
