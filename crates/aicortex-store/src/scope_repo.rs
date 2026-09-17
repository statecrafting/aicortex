//! Scopes: the identity every statement predicates on (spec 012 B-3, B-9).
//!
//! The predecessor ran its functions as the database service role and
//! bypassed row security entirely, so isolation depended on every call site
//! remembering to filter (`openbrain://service-role-everywhere`). Here the
//! scope is a parameter of every repository call, and the value it resolves
//! to is a [`ScopeId`] the caller cannot forge into another scope's: it is a
//! digest of the owner, the kind and the key, so two scopes collide only if
//! SHA-256 does.
//!
//! Deriving the id rather than generating one is what lets a capture be one
//! transaction (B-4): the writer knows the scope's id without reading the
//! scope table first, so the memory row and the row it references can be
//! staged into the same batch.

use core::fmt;

use aicortex_types::{Scope, ScopeKind};
use rahi_store::{Statement, StoreHandle, TxnBuilder, Value};
use rahi_types::{Error, UnixSeconds};
use serde::Deserialize;

use crate::hex_digest;

/// The identity of a scope in the schema: a derived, stable digest.
///
/// Derived from `owner`, the kind's label, and the key (the empty string for
/// a personal scope), separated by a byte that cannot occur in any of them,
/// so `("a", personal)` and `("a/personal", ...)` cannot collide by
/// concatenation.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ScopeId(String);

impl ScopeId {
    /// The separator: a unit separator, which [`aicortex_types`] refuses in
    /// every key it validates (a control character).
    const SEPARATOR: u8 = 0x1f;

    /// Derive the id of `scope`.
    #[must_use]
    pub fn of(scope: &Scope) -> Self {
        let mut material = Vec::new();
        material.extend_from_slice(scope.owner.as_str().as_bytes());
        material.push(Self::SEPARATOR);
        material.extend_from_slice(scope.kind.label().as_bytes());
        material.push(Self::SEPARATOR);
        material.extend_from_slice(Self::key_of(scope).as_bytes());
        Self(hex_digest(&material))
    }

    /// The key column's value: the kind's key, or the empty string.
    ///
    /// Never `NULL`: SQLite treats two `NULL`s in a unique index as distinct,
    /// so a nullable key would admit two personal scopes for one owner.
    #[must_use]
    pub fn key_of(scope: &Scope) -> &str {
        scope.kind.key().unwrap_or("")
    }

    /// The id as the schema stores it.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ScopeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&ScopeId> for Value {
    fn from(id: &ScopeId) -> Self {
        Self::Text(id.0.clone())
    }
}

/// A scope as the `scope` table holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScopeRow {
    /// The derived id.
    pub id: ScopeId,
    /// The scope itself, rebuilt from the row.
    pub scope: Scope,
    /// When this scope was first written to.
    pub created: UnixSeconds,
}

/// The `scope` table.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScopeRepo;

const ENSURE_SQL: &str = "INSERT OR IGNORE INTO scope (scope_id, owner, kind, key, created)
    VALUES ($1, $2, $3, $4, $5)";

const GET_SQL: &str = "SELECT scope_id, owner, kind, key, created FROM scope
    WHERE scope_id = $1";

#[derive(Debug, Deserialize)]
struct Row {
    scope_id: String,
    owner: String,
    kind: String,
    key: String,
    created: i64,
}

impl ScopeRepo {
    /// Stage the scope's row in the caller's batch (B-4).
    ///
    /// `INSERT OR IGNORE`, because a capture into an existing scope is the
    /// ordinary case and must not fail: the row is a fact about the scope,
    /// not about this write. It is staged rather than executed so that a
    /// capture stays one transaction even when it is the first one into a
    /// scope.
    pub fn ensure(txn: &mut TxnBuilder, scope: &Scope, created: UnixSeconds) {
        let id = ScopeId::of(scope);
        txn.push(Statement::with_params(
            ENSURE_SQL,
            vec![
                Value::from(&id),
                Value::from(scope.owner.as_str()),
                Value::from(scope.kind.label()),
                Value::from(ScopeId::key_of(scope)),
                Value::Integer(seconds_to_sql(created)),
            ],
        ));
    }

    /// The scope's row, if this scope has ever been written to.
    ///
    /// `query_consistent`: this is the read that decides whether a scope
    /// exists, and admission decisions (013) are taken on it, so a stale
    /// local replica would be a wrong answer rather than a late one (B-5).
    ///
    /// # Errors
    ///
    /// The store's error, or [`Error::Integrity`] when the row holds a kind
    /// or a key this crate cannot map back to a [`Scope`].
    pub async fn get(store: &StoreHandle, scope: &Scope) -> Result<Option<ScopeRow>, Error> {
        let id = ScopeId::of(scope);
        let rows: Vec<Row> = store
            .query_consistent(GET_SQL, vec![Value::from(&id)])
            .await?;
        rows.into_iter().next().map(row_to_scope).transpose()
    }
}

/// Rebuild a [`ScopeRow`] from its columns.
fn row_to_scope(row: Row) -> Result<ScopeRow, Error> {
    let owner = rahi_types::Sub::new(row.owner);
    let kind = match row.kind.as_str() {
        "personal" => ScopeKind::Personal,
        "project" => ScopeKind::Project {
            project: aicortex_types::ProjectKey::new(row.key)?,
        },
        "shared" => ScopeKind::Shared {
            share: aicortex_types::ShareKey::new(row.key)?,
        },
        other => {
            return Err(Error::Integrity(format!(
                "scope row {} holds the kind {other:?}, which is not one of the three",
                row.scope_id
            )));
        }
    };
    let created = u64::try_from(row.created).map_err(|_| {
        Error::Integrity(format!(
            "scope row {} holds a negative created time",
            row.scope_id
        ))
    })?;
    Ok(ScopeRow {
        id: ScopeId(row.scope_id),
        scope: Scope { owner, kind },
        created: UnixSeconds::new(created),
    })
}

/// A timestamp as the `i64` SQLite stores.
///
/// Saturates rather than wrapping, for the reason rahi's outbox gives about
/// revisions: a second beyond `i64::MAX` is unreachable, and a negative one
/// would sort before every real timestamp.
pub(crate) fn seconds_to_sql(seconds: UnixSeconds) -> i64 {
    i64::try_from(seconds.get()).unwrap_or(i64::MAX)
}
