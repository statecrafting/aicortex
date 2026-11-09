//! The wire types of `/api/v1` (B-10), and the only ones in the workspace.
//!
//! Inputs reject unknown fields; outputs are additive within the version, so
//! a client ignores a field it does not know. No output type here carries a
//! memory body: content leaves only inside the [`FramedMemories`] of 019,
//! which is flattened into [`MemoriesResponse`] so the framing statement sits
//! beside the envelopes (B-5). The bare fields beside it (ids, kinds, states,
//! timestamps) are not derived from stored text (019 B-6).

use aicortex_recall::FramedMemories;
use aicortex_store::{Erased, ScopeStats, StatusFilter};
use aicortex_types::{Memory, MemoryId, MemoryKind, ProjectKey, Scope, ShareKey, Status};
use rahi_types::Sub;
use serde::{Deserialize, Serialize};

use crate::error::{ApiError, ApiResult};

/// A scope as a request names it (D-4).
///
/// `owner` defaults to the caller. A scope is reachable when the caller owns
/// it; no grant table exists yet (012 B-9), so "plus those explicitly granted"
/// of B-2 grants nothing today.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ScopeRef {
    /// `personal`, `project`, or `shared`.
    pub kind: String,
    /// The project key, for a project scope.
    #[serde(default)]
    pub project: Option<String>,
    /// The share key, for a shared scope.
    #[serde(default)]
    pub share: Option<String>,
    /// The owning subject; the caller when absent.
    #[serde(default)]
    pub owner: Option<String>,
}

impl ScopeRef {
    /// The personal scope of the caller.
    #[must_use]
    pub fn personal() -> Self {
        Self {
            kind: "personal".to_owned(),
            project: None,
            share: None,
            owner: None,
        }
    }

    /// The scope this names, if `caller` can reach it (B-2).
    ///
    /// A malformed scope is a 400; a well-formed scope the caller cannot
    /// reach is a 404 decided here, before any store read, so neither the
    /// body nor the timing says whether the scope holds anything (FR-004).
    ///
    /// # Errors
    ///
    /// [`ApiError::invalid`] or [`ApiError::not_found`].
    pub fn resolve(&self, caller: &Sub) -> ApiResult<Scope> {
        let key = |value: &Option<String>, field: &str| {
            value.clone().ok_or_else(|| {
                ApiError::invalid(format!("a {} scope names its {field}", self.kind))
            })
        };
        let stray =
            |field: &str| ApiError::invalid(format!("a {} scope has no {field}", self.kind));
        let owner = self
            .owner
            .as_deref()
            .map_or_else(|| caller.clone(), Sub::new);
        let scope = match self.kind.as_str() {
            "personal" => {
                if self.project.is_some() {
                    return Err(stray("project"));
                }
                if self.share.is_some() {
                    return Err(stray("share"));
                }
                Scope::personal(owner)
            }
            "project" => {
                if self.share.is_some() {
                    return Err(stray("share"));
                }
                let project = ProjectKey::new(key(&self.project, "project")?)
                    .map_err(|error| ApiError::invalid(error.to_string()))?;
                Scope::project(owner, project)
            }
            "shared" => {
                if self.project.is_some() {
                    return Err(stray("project"));
                }
                let share = ShareKey::new(key(&self.share, "share")?)
                    .map_err(|error| ApiError::invalid(error.to_string()))?;
                Scope::shared(owner, share)
            }
            other => {
                return Err(ApiError::invalid(format!(
                    "scope.kind {other:?} is not one of personal, project, shared"
                )));
            }
        };
        if &scope.owner == caller {
            Ok(scope)
        } else {
            Err(ApiError::not_found())
        }
    }
}

/// The scope of a `GET` or `DELETE`, from the query string.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ScopeQuery {
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
}

impl ScopeQuery {
    /// The same scope as a [`ScopeRef`].
    #[must_use]
    pub fn scope_ref(&self) -> ScopeRef {
        ScopeRef {
            kind: self.scope.clone().unwrap_or_else(|| "personal".to_owned()),
            project: self.project.clone(),
            share: self.share.clone(),
            owner: self.owner.clone(),
        }
    }
}

/// `GET /memories`: a scope, an optional filter, and a keyset page.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ListQuery {
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
    /// Only memories in this state.
    #[serde(default)]
    pub status: Option<String>,
    /// Only memories of this kind.
    #[serde(default)]
    pub kind: Option<String>,
    /// The opaque cursor the previous page returned.
    #[serde(default)]
    pub cursor: Option<String>,
    /// Rows per page.
    #[serde(default)]
    pub limit: Option<u32>,
}

impl ListQuery {
    /// The scope this listing names.
    #[must_use]
    pub fn scope_ref(&self) -> ScopeRef {
        ScopeQuery {
            scope: self.scope.clone(),
            project: self.project.clone(),
            share: self.share.clone(),
            owner: self.owner.clone(),
        }
        .scope_ref()
    }

    /// The state filter, parsed.
    ///
    /// # Errors
    ///
    /// [`ApiError::invalid`] for a state outside the closed vocabulary.
    pub fn status_filter(&self) -> ApiResult<Option<StatusFilter>> {
        let Some(status) = self.status.as_deref() else {
            return Ok(None);
        };
        let filter = match status {
            "active" => StatusFilter::Active,
            "quarantined" => StatusFilter::Quarantined,
            "superseded" => StatusFilter::Superseded,
            "expired" => StatusFilter::Expired,
            "erased" => StatusFilter::Erased,
            other => {
                return Err(ApiError::invalid(format!(
                    "status {other:?} is not a state"
                )));
            }
        };
        Ok(Some(filter))
    }

    /// The kind filter, parsed.
    ///
    /// # Errors
    ///
    /// [`ApiError::invalid`] for a kind outside the closed vocabulary.
    pub fn kind_filter(&self) -> ApiResult<Option<MemoryKind>> {
        self.kind
            .as_deref()
            .map(|kind| {
                kind.parse::<MemoryKind>()
                    .map_err(|error| ApiError::invalid(error.to_string()))
            })
            .transpose()
    }
}

/// `DELETE /memories/{id}`: the scope, and whether to cascade (014 B-8).
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EraseQuery {
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
    /// Erase what was derived from this memory too.
    #[serde(default)]
    pub cascade: bool,
}

impl EraseQuery {
    /// The scope this erasure names.
    #[must_use]
    pub fn scope_ref(&self) -> ScopeRef {
        ScopeQuery {
            scope: self.scope.clone(),
            project: self.project.clone(),
            share: self.share.clone(),
            owner: self.owner.clone(),
        }
        .scope_ref()
    }
}

/// Where a candidate came from, as the client names it.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceIn {
    /// The source system; the presenting client's id when absent.
    #[serde(default)]
    pub system: Option<String>,
    /// The id of the item in the source system.
    #[serde(default)]
    pub external_id: Option<String>,
    /// Where the item can be found again.
    #[serde(default)]
    pub locator: Option<String>,
}

/// One candidate memory.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CandidateIn {
    /// What kind of claim it is.
    pub kind: MemoryKind,
    /// What it says.
    pub text: String,
    /// A short name.
    #[serde(default)]
    pub title: Option<String>,
    /// Where it came from.
    #[serde(default)]
    pub source: Option<SourceIn>,
    /// When the source captured it, in Unix seconds; now when absent.
    #[serde(default)]
    pub captured_at: Option<u64>,
}

/// `POST /capture`: one or many candidates into one scope (B-4).
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CaptureRequest {
    /// The scope every candidate lands in; the caller's personal scope when
    /// absent.
    #[serde(default)]
    pub scope: Option<ScopeRef>,
    /// The candidates, at least one and at most the batch bound (B-9).
    pub candidates: Vec<CandidateIn>,
}

/// What the gate and the store did with one candidate.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct CaptureOutcome {
    /// The candidate's position in the request.
    pub index: usize,
    /// `admit`, `quarantine`, or `refuse`.
    pub verdict: String,
    /// The id the store holds the content under: the created row's, or the
    /// existing row's on a merge. Absent on a refusal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<MemoryId>,
    /// Whether the content was already present and merged (014 B-2).
    pub merged: bool,
    /// The gate's reason code, for a quarantine or a refusal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<String>,
    /// The problem `type` of a refusal (B-7).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub problem: Option<String>,
    /// The ledger decision a quarantine or a refusal was recorded under.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<String>,
}

/// The answer to `POST /capture`.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct CaptureResponse {
    /// One outcome per candidate, in request order.
    pub results: Vec<CaptureOutcome>,
}

/// `PATCH /memories/{id}`: a correction or a supersession (B-4).
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PatchRequest {
    /// The scope the memory lives in; the caller's personal scope when absent.
    #[serde(default)]
    pub scope: Option<ScopeRef>,
    /// The correcting text: a new correction memory supersedes this one.
    #[serde(default)]
    pub correction: Option<CorrectionIn>,
    /// An existing memory of the same scope that supersedes this one.
    #[serde(default)]
    pub superseded_by: Option<MemoryId>,
}

/// The correcting content of a `PATCH`.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CorrectionIn {
    /// What the memory should have said.
    pub text: String,
    /// A short name.
    #[serde(default)]
    pub title: Option<String>,
}

/// The answer to a `PATCH`.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct PatchResponse {
    /// The memory that was superseded.
    pub id: MemoryId,
    /// The memory that supersedes it.
    pub superseded_by: MemoryId,
    /// Whether a correction's content was already present and merged.
    pub merged: bool,
}

/// What a listing says about one memory beside its envelope (019 B-6).
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct MemoryMeta {
    /// The memory's id, which also names its envelope.
    pub id: MemoryId,
    /// What kind of claim it is.
    pub kind: MemoryKind,
    /// Its state.
    pub status: String,
    /// The memory that superseded it, when one did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<MemoryId>,
    /// When it was written, in Unix seconds.
    pub created: u64,
    /// When it last changed, in Unix seconds.
    pub updated: u64,
}

impl MemoryMeta {
    /// The bare fields of `memory`.
    #[must_use]
    pub fn of(memory: &Memory) -> Self {
        Self {
            id: memory.id,
            kind: memory.kind,
            status: memory.status.label().to_owned(),
            superseded_by: match memory.status {
                Status::Superseded(next) => Some(next),
                _ => None,
            },
            created: memory.created.get(),
            updated: memory.updated.get(),
        }
    }
}

/// The answer to `GET /memories` and `GET /memories/{id}` (B-5).
#[derive(Serialize)]
pub struct MemoriesResponse {
    /// The framing statement and the envelopes, as 019 serializes them.
    #[serde(flatten)]
    pub framed: FramedMemories,
    /// The bare fields of each memory, in envelope order.
    pub items: Vec<MemoryMeta>,
    /// The cursor of the next page, when there is one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
}

/// The answer to `GET /stats`: the scope's counters (012 B-6).
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct StatsResponse {
    /// Every memory the scope holds, in any state.
    pub total: u64,
    /// How many of each kind.
    pub by_kind: std::collections::BTreeMap<String, u64>,
    /// How many in each state.
    pub by_status: std::collections::BTreeMap<String, u64>,
}

impl From<ScopeStats> for StatsResponse {
    fn from(stats: ScopeStats) -> Self {
        Self {
            total: stats.total,
            by_kind: stats.by_kind,
            by_status: stats.by_status,
        }
    }
}

/// The answer to `DELETE /memories/{id}` (014 B-7).
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct EraseResponse {
    /// The erased memory.
    pub id: MemoryId,
    /// Derivative rows removed.
    pub removed_derivatives: u64,
    /// Decision digest keys destroyed.
    pub keys_destroyed: u64,
    /// Derived memories marked `origin_erased`.
    pub marked: u64,
    /// Derived memories erased with it, when the erasure cascaded.
    pub cascaded: Vec<MemoryId>,
    /// The ledger decision recording the erasure.
    pub decision: String,
    /// What erasure does not reach (014 B-11).
    pub caveat: String,
}

impl From<Erased> for EraseResponse {
    fn from(erased: Erased) -> Self {
        Self {
            id: erased.memory,
            removed_derivatives: erased.removed_derivatives,
            keys_destroyed: erased.keys_destroyed,
            marked: erased.marked,
            cascaded: erased.cascaded,
            decision: erased.decision.to_string(),
            caveat: Erased::caveat().to_owned(),
        }
    }
}
