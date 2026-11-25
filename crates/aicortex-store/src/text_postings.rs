//! The text index's tables and the statements that keep them true when
//! chunks go away (spec 016 B-5, D-9).
//!
//! `chunk_token` holds one posting per distinct token per chunk, and
//! `token_df` the per-scope, per-revision document frequency and collection
//! frequency of each token. The empty token is reserved: every indexed chunk
//! carries one posting for it whose `tf` is the chunk's length in tokens, so
//! its `token_df` row is the chunk count and the total length BM25 needs.
//!
//! The tables live here, below the index crate, for the reason spec 015's
//! do: erasure (spec 014) is in this crate and must reach them, and the
//! crate graph points downward. Postings are *written* by `aicortex-index`,
//! which owns tokenization; they are *released* here, by every path that
//! removes a chunk, so `token_df` never counts a chunk that is gone and
//! never keeps a token only an erased memory contained.

use rahi_store::{Migration, Statement, TxnBuilder, Value};

/// The text index of spec 016 B-5, appended under an `extends` edge on
/// spec 012's migration list.
pub const TEXT_INDEX_VERSION: u32 = 10;

/// The token whose posting records a chunk's length and whose frequency row
/// records the chunk count and total length. Tokenization never emits it.
pub const STATS_TOKEN: &str = "";

const TEXT_INDEX_TABLES: &str = "CREATE TABLE chunk_token (
    chunk_id TEXT NOT NULL,
    scope_id TEXT NOT NULL,
    memory_id TEXT NOT NULL,
    model_revision INTEGER NOT NULL,
    token TEXT NOT NULL,
    tf INTEGER NOT NULL CHECK (tf >= 0),
    PRIMARY KEY (chunk_id, token)
);
CREATE INDEX chunk_token_postings
    ON chunk_token (scope_id, model_revision, token, chunk_id);
CREATE INDEX chunk_token_memory
    ON chunk_token (scope_id, memory_id, model_revision);
CREATE TABLE token_df (
    scope_id TEXT NOT NULL,
    model_revision INTEGER NOT NULL,
    token TEXT NOT NULL,
    df INTEGER NOT NULL CHECK (df > 0),
    cf INTEGER NOT NULL CHECK (cf >= 0),
    PRIMARY KEY (scope_id, model_revision, token)
)";

/// Spec 016's text index migration.
///
/// Not additive (spec 046 B-2): an older binary does not release postings
/// when it erases a memory, so serving one across this version could keep
/// the tokens of erased content in `token_df`. Rollback across it is
/// refused, as for spec 015's version 9 (015 D-11).
#[must_use]
pub fn text_index_migration() -> Migration {
    Migration::new(
        TEXT_INDEX_VERSION,
        "aicortex text index postings and token frequencies",
        TEXT_INDEX_TABLES,
    )
}

/// Subtract `memory`'s postings from `token_df` and drop the frequency rows
/// that reach zero, at one revision or (with `None`) at all of them.
///
/// Stage it before the postings themselves are deleted: it reads them. The
/// erasure sweep deletes the postings afterwards as an ordinary derivative;
/// the embedding worker deletes them with [`stage_delete_memory_postings`].
pub fn stage_uncount_memory(
    txn: &mut TxnBuilder,
    scope_id: &str,
    memory_id: &str,
    revision: Option<u32>,
) {
    let params = || {
        vec![
            Value::from(scope_id),
            Value::from(memory_id),
            revision.map_or(Value::Null, |revision| Value::Integer(i64::from(revision))),
        ]
    };
    // Rows this memory accounts for entirely go first, while their `df`
    // is still the count to compare against; the survivors are then
    // decremented.
    txn.push(Statement::with_params(
        "DELETE FROM token_df
         WHERE scope_id = ?1 AND (model_revision, token) IN (
             SELECT model_revision, token FROM chunk_token
             WHERE scope_id = ?1 AND memory_id = ?2
               AND (?3 IS NULL OR model_revision = ?3)
             GROUP BY model_revision, token
             HAVING COUNT(*) >= (SELECT f.df FROM token_df f
                                 WHERE f.scope_id = ?1
                                   AND f.model_revision = chunk_token.model_revision
                                   AND f.token = chunk_token.token))",
        params(),
    ));
    txn.push(Statement::with_params(
        "UPDATE token_df SET df = token_df.df - gone.n, cf = token_df.cf - gone.tf
         FROM (SELECT model_revision, token, COUNT(*) AS n, SUM(tf) AS tf
               FROM chunk_token
               WHERE scope_id = ?1 AND memory_id = ?2
                 AND (?3 IS NULL OR model_revision = ?3)
               GROUP BY model_revision, token) AS gone
         WHERE token_df.scope_id = ?1 AND token_df.model_revision = gone.model_revision
           AND token_df.token = gone.token AND token_df.df > gone.n",
        params(),
    ));
}

/// Delete `memory`'s postings at one revision or (with `None`) all of them.
///
/// Stage [`stage_uncount_memory`] first.
pub fn stage_delete_memory_postings(
    txn: &mut TxnBuilder,
    scope_id: &str,
    memory_id: &str,
    revision: Option<u32>,
) {
    txn.push(Statement::with_params(
        "DELETE FROM chunk_token
         WHERE scope_id = ?1 AND memory_id = ?2 AND (?3 IS NULL OR model_revision = ?3)",
        vec![
            Value::from(scope_id),
            Value::from(memory_id),
            revision.map_or(Value::Null, |revision| Value::Integer(i64::from(revision))),
        ],
    ));
}

/// Delete every posting and frequency row of one inactive revision in a
/// scope, as spec 015's `drop_revision` deletes its chunks and vectors.
///
/// Guarded the same way: nothing is deleted unless the revision is
/// recorded inactive.
pub fn stage_drop_revision(txn: &mut TxnBuilder, scope_id: &str, revision: u32) {
    for sql in [
        "DELETE FROM chunk_token WHERE scope_id = ?1 AND model_revision = ?2
         AND EXISTS (SELECT 1 FROM embedding_model WHERE revision = ?2 AND active = 0)",
        "DELETE FROM token_df WHERE scope_id = ?1 AND model_revision = ?2
         AND EXISTS (SELECT 1 FROM embedding_model WHERE revision = ?2 AND active = 0)",
    ] {
        txn.push(Statement::with_params(
            sql,
            vec![Value::from(scope_id), Value::Integer(i64::from(revision))],
        ));
    }
}
