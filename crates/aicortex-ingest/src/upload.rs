//! The staging area for uploaded exports (spec 031 B-1).
//!
//! An export is a file the user uploads, never an archive fetched from a third
//! party on their behalf, so nothing here holds a credential for another
//! service. A file is staged under a directory its scope owns, read by an
//! adapter through the same scope, and deleted when the run that read it ends
//! or when it expires, whichever comes first. The scope is part of the path:
//! an upload id from another scope names nothing.
//!
//! The routes are `POST /uploads` and `DELETE /uploads/{id}`. They reach the
//! API through its extension seam (030 D-2), not through an edit of its router.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use aicortex_api::dto::ScopeQuery;
use aicortex_api::{Api, ApiError, ApiResult, ApiRoute, ApiScope, Caller};
use aicortex_store::ScopeId;
use aicortex_types::Scope;
use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::QueryRejection;
use axum::extract::{DefaultBodyLimit, Path as UrlPath, Query, State};
use axum::http::StatusCode;
use axum::routing::{delete, post};
use rahi_types::Error;
use serde::Serialize;

/// The largest upload, in bytes (D-3).
pub const MAX_UPLOAD_BYTES: usize = 512 * 1024 * 1024;

/// How long a staged file lives before a sweep deletes it.
pub const DEFAULT_TTL: Duration = Duration::from_secs(24 * 60 * 60);

const PREFIX: &str = "up-";

/// A staged upload's name: `up-` and a v7 UUID, the only shape a path
/// segment under the staging root can take.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UploadId(String);

impl UploadId {
    fn mint() -> Self {
        Self(format!("{PREFIX}{}", uuid::Uuid::now_v7().simple()))
    }

    /// Read an id back from a route or a run's options.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when the text is not exactly what a staging
    /// area mints, which also keeps a path separator out of every join.
    pub fn parse(text: &str) -> Result<Self, Error> {
        text.strip_prefix(PREFIX)
            .and_then(|rest| uuid::Uuid::parse_str(rest).ok())
            .map(|uuid| format!("{PREFIX}{}", uuid.simple()))
            .filter(|canonical| canonical == text)
            .map(Self)
            .ok_or_else(|| {
                Error::Validation("an upload id is up- and a 32 digit hex id".to_owned())
            })
    }

    /// The id.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl core::fmt::Display for UploadId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A directory of staged uploads, one subdirectory per scope.
#[derive(Clone, Debug)]
pub struct Staging {
    root: PathBuf,
    ttl: Duration,
}

impl Staging {
    /// A staging area under `root`, which is created when first written.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            ttl: DEFAULT_TTL,
        }
    }

    /// The same area with another lifetime for a staged file.
    #[must_use]
    pub const fn with_ttl(mut self, ttl: Duration) -> Self {
        self.ttl = ttl;
        self
    }

    fn dir(&self, scope: &Scope) -> PathBuf {
        self.root.join(ScopeId::of(scope).as_str())
    }

    fn path(&self, scope: &Scope, id: &UploadId) -> PathBuf {
        self.dir(scope).join(id.as_str())
    }

    /// Stage `bytes` for `scope` and return its id. Sweeps expired files of
    /// the scope first, so expiry needs no task of its own.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] over the size ceiling or empty;
    /// [`Error::Internal`] when the file cannot be written.
    pub fn put(&self, scope: &Scope, bytes: &[u8]) -> Result<UploadId, Error> {
        if bytes.is_empty() || bytes.len() > MAX_UPLOAD_BYTES {
            return Err(Error::Validation(format!(
                "an upload is between 1 and {MAX_UPLOAD_BYTES} bytes"
            )));
        }
        self.sweep(scope);
        let dir = self.dir(scope);
        let io = |error: std::io::Error| Error::Io(format!("staging an upload: {error}"));
        fs::create_dir_all(&dir).map_err(io)?;
        let id = UploadId::mint();
        let path = dir.join(id.as_str());
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(io)?;
        if let Err(error) = file.write_all(bytes).and_then(|()| file.sync_all()) {
            drop(fs::remove_file(&path));
            return Err(io(error));
        }
        Ok(id)
    }

    /// The staged bytes of `id` in `scope`.
    ///
    /// # Errors
    ///
    /// [`Error::NotFound`] when the scope holds no such file (never staged,
    /// deleted, expired, or another scope's).
    pub fn read(&self, scope: &Scope, id: &UploadId) -> Result<Vec<u8>, Error> {
        let path = self.path(scope, id);
        if self.expired(&path) {
            drop(fs::remove_file(&path));
        }
        fs::read(&path).map_err(|_| Error::NotFound(format!("no upload {id}")))
    }

    /// Delete `id`; whether a file was there.
    pub fn delete(&self, scope: &Scope, id: &UploadId) -> bool {
        fs::remove_file(self.path(scope, id)).is_ok()
    }

    /// Delete the scope's files older than the lifetime; how many.
    pub fn sweep(&self, scope: &Scope) -> usize {
        let Ok(entries) = fs::read_dir(self.dir(scope)) else {
            return 0;
        };
        entries
            .flatten()
            .filter(|entry| self.expired(&entry.path()) && fs::remove_file(entry.path()).is_ok())
            .count()
    }

    fn expired(&self, path: &Path) -> bool {
        fs::metadata(path)
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|modified| SystemTime::now().duration_since(modified).ok())
            .is_some_and(|age| age > self.ttl)
    }
}

/// What a stage answers.
#[derive(Debug, Serialize)]
struct Staged {
    upload_id: String,
    bytes: usize,
    expires_in_seconds: u64,
}

/// The routes of this module, for the app to hand to `Api::with_routes`.
#[must_use]
pub fn routes(staging: Arc<Staging>) -> Vec<ApiRoute> {
    let put = {
        let staging = Arc::clone(&staging);
        move |State(api): State<Api>,
              caller: Caller,
              scope: Result<Query<ScopeQuery>, QueryRejection>,
              body: Bytes| {
            let staging = Arc::clone(&staging);
            async move { put(&staging, &api, &caller, scope, body).await }
        }
    };
    let remove = move |State(api): State<Api>,
                       caller: Caller,
                       UrlPath(id): UrlPath<String>,
                       scope: Result<Query<ScopeQuery>, QueryRejection>| {
        let staging = Arc::clone(&staging);
        async move { remove(&staging, &api, &caller, &id, scope).await }
    };
    vec![
        ApiRoute::new(
            "/uploads",
            ApiScope::MemoryWrite,
            post(put).layer(DefaultBodyLimit::max(MAX_UPLOAD_BYTES)),
        ),
        ApiRoute::new("/uploads/{id}", ApiScope::MemoryWrite, delete(remove)),
    ]
}

fn scope_of(query: Result<Query<ScopeQuery>, QueryRejection>, caller: &Caller) -> ApiResult<Scope> {
    let Query(query) = query.map_err(|rejection| ApiError::invalid(rejection.body_text()))?;
    query.scope_ref().resolve(caller.sub())
}

async fn put(
    staging: &Arc<Staging>,
    _api: &Api,
    caller: &Caller,
    scope: Result<Query<ScopeQuery>, QueryRejection>,
    body: Bytes,
) -> ApiResult<(StatusCode, Json<Staged>)> {
    let scope = scope_of(scope, caller)?;
    let (area, bytes) = (Arc::clone(staging), body.len());
    let id = tokio::task::spawn_blocking(move || area.put(&scope, &body))
        .await
        .map_err(|_| ApiError::invalid("the upload could not be staged"))?
        .map_err(|error| match error {
            Error::Validation(message) => ApiError::invalid(message),
            _ => ApiError::invalid("the upload could not be staged"),
        })?;
    Ok((
        StatusCode::CREATED,
        Json(Staged {
            upload_id: id.to_string(),
            bytes,
            expires_in_seconds: staging.ttl.as_secs(),
        }),
    ))
}

async fn remove(
    staging: &Arc<Staging>,
    _api: &Api,
    caller: &Caller,
    id: &str,
    scope: Result<Query<ScopeQuery>, QueryRejection>,
) -> ApiResult<StatusCode> {
    let id = UploadId::parse(id).map_err(|_| ApiError::not_found())?;
    let scope = scope_of(scope, caller)?;
    if staging.delete(&scope, &id) {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found())
    }
}
