//! The memory table, and the only code allowed to write it (spec 012 B-3,
//! B-4, B-5, B-7, B-8).
//!
//! Two properties hold here and are checked rather than remembered.
//!
//! **The scope is in the predicate.** Every method takes a [`Scope`] and
//! every statement it emits names `scope_id`. There is no method that returns
//! memories across scopes, no method that takes a SQL fragment from a caller,
//! and no post-filter: a row of another scope is not returned and then
//! dropped, it is never selected. The predecessor's isolation depended on
//! every call site remembering to filter
//! (`openbrain://service-role-everywhere`); a test in this crate greps for a
//! `SELECT` against these tables that does not name `scope_id` and fails on a
//! match (FR-007).
//!
//! **A capture is one transaction.** [`MemoryRepo::insert`] appends every
//! statement to the caller's [`TxnBuilder`]: the scope row, the memory, its
//! provenance, its derivation rows, its counter, and the outbox work. It
//! opens no transaction of its own and executes nothing, so a caller cannot
//! land the row and lose the work (constitution XI).

use aicortex_types::{Memory, MemoryId, MemoryKind, Provenance, Scope, Status};
use rahi_store::{Envelope, Outbox, Statement, StoreHandle, TxnBuilder, Value};
use rahi_types::{Error, UnixSeconds};
use serde::Deserialize;

use crate::counters::Counters;
use crate::cursor::{Cursor, CursorKey};
use crate::hex_digest;
use crate::provenance_repo::ProvenanceRepo;
use crate::scope_repo::{ScopeId, ScopeRepo, seconds_to_sql};

/// The default body ceiling: 64 KiB of text (B-8).
pub const DEFAULT_MAX_BODY_BYTES: usize = 64 * 1024;

/// The largest page a listing will return (B-7).
///
/// There is no unbounded select in this crate. A caller that asks for more
/// than this is refused rather than quietly served a different number,
/// because a silently clamped limit is a paging bug that only shows up under
/// load.
pub const MAX_PAGE_ROWS: u32 = 500;

/// The page size a surface uses when its client names none.
///
/// [`MemoryRepo::list`] takes its limit explicitly and applies no default of
/// its own: a storage call that silently chose a page size would hide the
/// cost of the read from the caller that pays it. The number is published
/// here so the surfaces of specs 020 and 021 share one default instead of
/// each inventing its own.
pub const DEFAULT_PAGE_ROWS: u32 = 100;

/// Which stored state a listing wants, as the column holds it.
///
/// A read-side projection of [`Status`] onto the discriminant the `status`
/// column carries: [`Status::Superseded`] names its successor, and a filter
/// asks for the state rather than for one particular successor.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StatusFilter {
    /// True, and visible to retrieval.
    Active,
    /// Stored, but its origin could not be established.
    Quarantined,
    /// Replaced by a later memory.
    Superseded,
    /// Past the moment it was true until.
    Expired,
    /// Forgotten.
    Erased,
}

impl StatusFilter {
    /// The discriminant, as the `status` column holds it.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Quarantined => "quarantined",
            Self::Superseded => "superseded",
            Self::Expired => "expired",
            Self::Erased => "erased",
        }
    }

    /// The state a memory is in, as a filter.
    #[must_use]
    pub const fn of(status: &Status) -> Self {
        match status {
            Status::Active => Self::Active,
            Status::Quarantined => Self::Quarantined,
            Status::Superseded(_) => Self::Superseded,
            Status::Expired => Self::Expired,
            Status::Erased => Self::Erased,
        }
    }
}

/// What a listing asks for besides its scope.
///
/// Closed by construction: there is no free-form predicate field and no way
/// to add one without changing this type, because a caller-supplied fragment
/// is how a scope predicate gets lost.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MemoryFilter {
    /// Only memories in this state, or every state when absent.
    pub status: Option<StatusFilter>,
    /// Only memories of this kind, or every kind when absent.
    pub kind: Option<MemoryKind>,
}

impl MemoryFilter {
    /// Everything the scope holds, in any state.
    #[must_use]
    pub const fn all() -> Self {
        Self {
            status: None,
            kind: None,
        }
    }

    /// What retrieval sees by default: the active memories.
    #[must_use]
    pub const fn active() -> Self {
        Self {
            status: Some(StatusFilter::Active),
            kind: None,
        }
    }

    /// The same filter, narrowed to one kind.
    #[must_use]
    pub const fn of_kind(mut self, kind: MemoryKind) -> Self {
        self.kind = Some(kind);
        self
    }

    /// A stable digest of what this filter asks for (B-7).
    ///
    /// Signed into every cursor, so a cursor from one filter cannot be
    /// presented with another. Canonical by construction: the two fields are
    /// rendered in a fixed order with a separator neither label can contain.
    #[must_use]
    pub fn digest(&self) -> String {
        let status = self.status.map_or("*", StatusFilter::label);
        let kind = self.kind.map_or("*", MemoryKind::label);
        hex_digest(format!("status={status}\x1fkind={kind}").as_bytes())
    }
}

/// One page of a listing, and the cursor that continues it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Listing {
    /// The memories in this page, newest first.
    pub memories: Vec<Memory>,
    /// The cursor the next page starts after, absent when this page is the
    /// last one. Opaque and signed (B-7).
    pub next: Option<String>,
}

/// The `memory` table.
///
/// Holds the body ceiling because B-8 makes it configurable; everything else
/// it needs is a parameter of the call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemoryRepo {
    max_body_bytes: usize,
}

impl Default for MemoryRepo {
    fn default() -> Self {
        Self {
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
        }
    }
}

const INSERT_SQL: &str = "INSERT INTO memory (
        id, scope_id, status, superseded_by, kind, trust, fingerprint,
        schema_version, body_bytes, created, updated, record
    ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)";

const GET_SQL: &str = "SELECT record FROM memory WHERE scope_id = $1 AND id = $2";

const FINGERPRINT_SQL: &str = "SELECT id FROM memory
    WHERE scope_id = $1 AND fingerprint = $2 AND status <> 'erased'";

/// The base of every listing: the scope, always, in the statement itself.
const LIST_BASE_SQL: &str = "SELECT record, created, id FROM memory WHERE scope_id = $1";

#[derive(Debug, Deserialize)]
struct RecordRow {
    record: String,
}

#[derive(Debug, Deserialize)]
struct ListRow {
    record: String,
    created: i64,
    id: String,
}

#[derive(Debug, Deserialize)]
struct IdRow {
    id: String,
}

impl MemoryRepo {
    /// A repository with the default body ceiling.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The same repository with a different ceiling (B-8).
    #[must_use]
    pub const fn with_max_body_bytes(mut self, bytes: usize) -> Self {
        self.max_body_bytes = bytes;
        self
    }

    /// The ceiling this repository enforces.
    #[must_use]
    pub const fn max_body_bytes(&self) -> usize {
        self.max_body_bytes
    }

    /// Stage a capture: the memory, its provenance, its derivation rows, its
    /// counter, and its outbox work, in the caller's transaction (B-4).
    ///
    /// Nothing is executed. The caller submits the batch with
    /// [`rahi_store::StoreHandle::txn`], which is what makes the five writes
    /// one atomic unit; a repository that opened its own transaction could
    /// not be composed with the caller's other work, and one that executed
    /// directly would land the row before the work that depends on it.
    ///
    /// `provenance` is the row that is written; it must be the memory's own.
    /// The record of 011 carries its provenance inline, and a signature that
    /// took a second one without checking would be a way to store a memory
    /// whose row and record disagree about where it came from (D-2).
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when `provenance` is not `memory.provenance`,
    /// when the body is over the ceiling (B-8), or when the record does not
    /// serialize.
    pub fn insert(
        &self,
        txn: &mut TxnBuilder,
        memory: &Memory,
        provenance: &Provenance,
        work: &Envelope,
    ) -> Result<(), Error> {
        if provenance != &memory.provenance {
            return Err(Error::Validation(format!(
                "the provenance offered for memory {} is not the one the record carries",
                memory.id
            )));
        }
        let body_bytes = memory.body.len();
        if body_bytes > self.max_body_bytes {
            return Err(Error::Validation(format!(
                "memory {} has a {body_bytes} byte body, over the {} byte ceiling",
                memory.id, self.max_body_bytes
            )));
        }
        let scope_id = ScopeId::of(&memory.scope);
        let record = serde_json::to_string(memory).map_err(|error| {
            Error::Validation(format!("memory {} does not serialize: {error}", memory.id))
        })?;
        let body_bytes = i64::try_from(body_bytes).map_err(|_| {
            Error::Validation(format!("memory {} has a body beyond i64", memory.id))
        })?;

        ScopeRepo::ensure(txn, &memory.scope, memory.created);
        txn.push(Statement::with_params(
            INSERT_SQL,
            vec![
                Value::from(memory.id.to_string()),
                Value::from(&scope_id),
                Value::from(memory.status.label()),
                Value::from(memory.status.superseded_by().map(|by| by.to_string())),
                Value::from(memory.kind.label()),
                Value::from(memory.trust.label()),
                Value::from(fingerprint(memory)),
                Value::from(u32::from(memory.schema_version)),
                Value::Integer(body_bytes),
                Value::Integer(seconds_to_sql(memory.created)),
                Value::Integer(seconds_to_sql(memory.updated)),
                Value::from(record),
            ],
        ));
        ProvenanceRepo::stage(txn, &scope_id, memory.id, provenance)?;
        Counters::increment(txn, &scope_id, memory.kind, &memory.status);
        Outbox::stage(txn, work);
        Ok(())
    }

    /// One memory of one scope.
    ///
    /// `query`, the local replica: a detail read shows what this replica has
    /// applied, and nothing is decided on it. Admission and uniqueness state
    /// their stronger need in [`Self::fingerprint_holder`] (B-5).
    ///
    /// # Errors
    ///
    /// The store's error, or [`Error::Integrity`] when the stored record does
    /// not read back as a memory.
    pub async fn get(
        &self,
        store: &StoreHandle,
        scope: &Scope,
        id: MemoryId,
    ) -> Result<Option<Memory>, Error> {
        let scope_id = ScopeId::of(scope);
        let rows: Vec<RecordRow> = store
            .query(
                GET_SQL,
                vec![Value::from(&scope_id), Value::from(id.to_string())],
            )
            .await?;
        rows.into_iter()
            .next()
            .map(|row| decode(&row.record))
            .transpose()
    }

    /// The memory that already holds this content's fingerprint, if any.
    ///
    /// `query_consistent`, the leader: this is a uniqueness read, and the
    /// answer decides whether a capture is admitted or merged (013, 014). A
    /// stale local replica would admit a duplicate that the unique index then
    /// refuses at commit, turning a clean decision into a constraint
    /// violation (B-5).
    ///
    /// # Errors
    ///
    /// The store's error, or [`Error::Integrity`] when a row holds an id that
    /// is not a memory id.
    pub async fn fingerprint_holder(
        &self,
        store: &StoreHandle,
        scope: &Scope,
        fingerprint: &str,
    ) -> Result<Option<MemoryId>, Error> {
        let scope_id = ScopeId::of(scope);
        let rows: Vec<IdRow> = store
            .query_consistent(
                FINGERPRINT_SQL,
                vec![Value::from(&scope_id), Value::from(fingerprint)],
            )
            .await?;
        rows.into_iter()
            .next()
            .map(|row| {
                MemoryId::parse(&row.id).map_err(|error| {
                    Error::Integrity(format!(
                        "memory holds an id that is not a memory id: {error}"
                    ))
                })
            })
            .transpose()
    }

    /// One page of a scope's memories, newest first (B-7).
    ///
    /// Ordered by `(created desc, id desc)`, which is total because ids are
    /// unique, and paged by the last pair seen rather than by an offset. A
    /// row inserted while a caller pages is either newer than the page it is
    /// on, in which case it sorts before the cursor and is simply not in this
    /// listing, or older, in which case it appears once in a later page.
    /// Nothing is returned twice and nothing is skipped because a count
    /// moved.
    ///
    /// `query`, the local replica: a listing is a report (B-5).
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when `limit` is zero or above
    /// [`MAX_PAGE_ROWS`], or when `cursor` was not signed by `key` for this
    /// scope and this filter; the store's error; [`Error::Integrity`] when a stored
    /// record does not read back.
    pub async fn list(
        &self,
        store: &StoreHandle,
        key: &CursorKey,
        scope: &Scope,
        filter: &MemoryFilter,
        limit: u32,
        cursor: Option<&str>,
    ) -> Result<Listing, Error> {
        if limit == 0 || limit > MAX_PAGE_ROWS {
            return Err(Error::Validation(format!(
                "a page of {limit} rows is not between 1 and {MAX_PAGE_ROWS}"
            )));
        }
        let scope_id = ScopeId::of(scope);
        let after = cursor
            .map(|text| Cursor::decode(text, key, &scope_id, filter))
            .transpose()?;

        let mut sql = String::from(LIST_BASE_SQL);
        let mut params = vec![Value::from(&scope_id)];
        if let Some(status) = filter.status {
            params.push(Value::from(status.label()));
            sql.push_str(&format!(" AND status = ${}", params.len()));
        }
        if let Some(kind) = filter.kind {
            params.push(Value::from(kind.label()));
            sql.push_str(&format!(" AND kind = ${}", params.len()));
        }
        if let Some(after) = after {
            params.push(Value::Integer(seconds_to_sql(after.created)));
            let created = params.len();
            params.push(Value::from(after.id.to_string()));
            let id = params.len();
            sql.push_str(&format!(
                " AND (created < ${created} OR (created = ${created} AND id < ${id}))"
            ));
        }
        params.push(Value::Integer(i64::from(limit)));
        sql.push_str(&format!(
            " ORDER BY created DESC, id DESC LIMIT ${}",
            params.len()
        ));

        let rows: Vec<ListRow> = store.query(sql, params).await?;
        let full = u32::try_from(rows.len()).is_ok_and(|read| read == limit);
        let last = rows.last().map(|row| (row.created, row.id.clone()));
        let memories = rows
            .into_iter()
            .map(|row| decode(&row.record))
            .collect::<Result<Vec<Memory>, Error>>()?;
        let next = match (full, last) {
            (true, Some((created, id))) => {
                let created = u64::try_from(created).map_err(|_| {
                    Error::Integrity("memory holds a negative created time".to_owned())
                })?;
                let id = MemoryId::parse(&id).map_err(|error| {
                    Error::Integrity(format!(
                        "memory holds an id that is not a memory id: {error}"
                    ))
                })?;
                Some(Cursor::new(UnixSeconds::new(created), id).encode(key, &scope_id, filter))
            }
            _ => None,
        };
        Ok(Listing { memories, next })
    }
}

/// Read a stored record back as the memory it is.
fn decode(record: &str) -> Result<Memory, Error> {
    serde_json::from_str(record)
        .map_err(|error| Error::Integrity(format!("a stored memory does not read back: {error}")))
}

/// The content fingerprint the `(scope_id, fingerprint)` index is unique on.
///
/// Spec 014 owns what content is normalized to before it is digested (its
/// `fingerprint.rs`, and the near-duplicate question that exact digesting
/// cannot answer). What 012 owns is the column, the partial unique index over
/// the memories that still exist, and that a duplicate is refused at the
/// storage boundary rather than by whoever remembered to look. Until 014
/// lands, the digest is over the kind and the body's text as written (D-4).
#[must_use]
pub fn fingerprint(memory: &Memory) -> String {
    let mut material = Vec::new();
    material.extend_from_slice(memory.kind.label().as_bytes());
    material.push(0x1f);
    material.extend_from_slice(memory.body.text.as_bytes());
    hex_digest(&material)
}
