//! The wire types of `/api/v1` (B-10), and the only ones in the workspace.
//!
//! Inputs reject unknown fields; outputs are additive within the version, so
//! a client ignores a field it does not know. No output type here carries a
//! memory body: content leaves only inside the envelope of 019 (B-5).

use aicortex_types::{MemoryId, MemoryKind, ProjectKey, Scope, ShareKey};
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
