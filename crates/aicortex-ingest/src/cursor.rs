//! Where a run resumes (spec 030 B-3).
//!
//! The cursor is the adapter's own bookmark, opaque to everything else. The
//! store holds one per `(scope, source)`, and it commits in the same
//! transaction as the items it covers, so an interrupted run resumes exactly
//! where it stopped: there is no window in which an item is stored and the
//! cursor still points before it, and none in which the cursor points past an
//! item that was not stored.

use aicortex_store::ScopeId;
use aicortex_types::Scope;
use rahi_store::{Statement, StoreHandle, TxnBuilder, Value};
use rahi_types::{Error, UnixSeconds};
use serde::Deserialize;

use crate::adapter::SourceId;

/// The longest cursor, in bytes.
pub const MAX_CURSOR_BYTES: usize = 4_096;

/// An adapter's bookmark.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cursor(String);

impl Cursor {
    /// Validate and wrap a bookmark.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] for an empty cursor, one over
    /// [`MAX_CURSOR_BYTES`], or one carrying a control character. A cursor
    /// comes from outside the product and is stored and handed back, so its
    /// size is bounded here and not by whoever reads it.
    pub fn new(value: impl Into<String>) -> Result<Self, Error> {
        let value = value.into();
        if value.is_empty() {
            return Err(Error::Validation("a cursor is not empty".to_owned()));
        }
        if value.len() > MAX_CURSOR_BYTES {
            return Err(Error::Validation(format!(
                "a cursor is at most {MAX_CURSOR_BYTES} bytes, not {}",
                value.len()
            )));
        }
        if value.chars().any(char::is_control) {
            return Err(Error::Validation(
                "a cursor carries no control character".to_owned(),
            ));
        }
        Ok(Self(value))
    }

    /// The bookmark.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Deserialize)]
struct CursorRow {
    cursor: String,
}

/// The `ingest_cursor` table.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CursorRepo;

impl CursorRepo {
    /// The saved cursor of one source in one scope, if a run left one.
    ///
    /// `query_consistent`, the leader: a run decides where to start on this,
    /// and a stale replica would start it behind the last committed page and
    /// replay items that were already stored.
    ///
    /// # Errors
    ///
    /// The store's error; [`Error::Integrity`] when the stored cursor is not
    /// one this crate would have written.
    pub async fn load(
        store: &StoreHandle,
        scope: &Scope,
        source: &SourceId,
    ) -> Result<Option<Cursor>, Error> {
        let rows: Vec<CursorRow> = store
            .query_consistent(
                "SELECT cursor FROM ingest_cursor WHERE scope_id = $1 AND source_id = $2",
                vec![
                    Value::from(&ScopeId::of(scope)),
                    Value::from(source.as_str()),
                ],
            )
            .await?;
        rows.into_iter()
            .next()
            .map(|row| {
                Cursor::new(row.cursor).map_err(|error| {
                    Error::Integrity(format!("the stored cursor does not read back: {error}"))
                })
            })
            .transpose()
    }

    /// Stage the cursor a run has reached, in the caller's transaction.
    ///
    /// An upsert: the guard that opens the transaction has already compared
    /// the stored cursor with the one the page was fetched from, so this
    /// statement writes over a value known to be the expected one.
    pub fn stage_advance(
        txn: &mut TxnBuilder,
        scope: &Scope,
        source: &SourceId,
        cursor: &Cursor,
        run: &str,
        at: UnixSeconds,
    ) {
        txn.push(Statement::with_params(
            "INSERT INTO ingest_cursor (scope_id, source_id, cursor, run_id, touched)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (scope_id, source_id) DO UPDATE
                SET cursor = excluded.cursor, run_id = excluded.run_id,
                    touched = excluded.touched",
            vec![
                Value::from(&ScopeId::of(scope)),
                Value::from(source.as_str()),
                Value::from(cursor.as_str()),
                Value::from(run),
                Value::Integer(seconds(at)),
            ],
        ));
    }

    /// Stage the removal of the saved cursor: the run reached the end, so the
    /// next run starts from the beginning.
    pub fn stage_clear(txn: &mut TxnBuilder, scope: &Scope, source: &SourceId) {
        txn.push(Statement::with_params(
            "DELETE FROM ingest_cursor WHERE scope_id = ?1 AND source_id = ?2",
            vec![
                Value::from(&ScopeId::of(scope)),
                Value::from(source.as_str()),
            ],
        ));
    }
}

/// Unix seconds as the signed integer SQLite stores, saturating.
pub(crate) fn seconds(at: UnixSeconds) -> i64 {
    i64::try_from(at.get()).unwrap_or(i64::MAX)
}
