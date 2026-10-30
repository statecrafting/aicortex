//! The text channel: an application-owned inverted index (spec 016 B-5).
//!
//! FTS5 is not compiled into hiqlite's SQLite (D-1), so postings are
//! ordinary rows. [`stage_chunk`] writes one chunk's postings and folds them
//! into `token_df` inside the caller's transaction, which for spec 015's
//! worker is the transaction that writes the chunk. [`search`] scores the
//! query's tokens with BM25 over the posting lists, with the scope, the
//! revision, and the status filter as predicates in the statement.
//!
//! The tables and the statements that release postings when a chunk goes
//! away are `aicortex_store::text_postings`, so erasure reaches them.

use std::collections::{BTreeMap, HashMap};
use std::time::Instant;

use aicortex_store::ScopeId;
use aicortex_store::text_postings::STATS_TOKEN;
use aicortex_types::MemoryId;
use rahi_store::{Page, Statement, StoreHandle, TxnBuilder, Value};
use rahi_types::Error;
use serde::Deserialize;

use crate::scan::{Filter, ScanConfig, ScanOutcome};
use crate::tokenize::{TokenizerConfig, tokenize};
use crate::topk::{Candidate, TopK};

/// BM25's term-frequency saturation, the standard value.
pub const BM25_K1: f64 = 1.2;
/// BM25's length normalization, the standard value.
pub const BM25_B: f64 = 0.75;

/// One chunk's text, as spec 015's worker writes it.
#[derive(Clone, Copy, Debug)]
pub struct ChunkText<'a> {
    /// The scope the chunk's memory lives in.
    pub scope_id: &'a str,
    /// The memory the chunk belongs to.
    pub memory_id: MemoryId,
    /// The model revision the chunk was cut for.
    pub revision: u32,
    /// The chunk id spec 015 assigned.
    pub chunk_id: &'a str,
    /// The chunk's text.
    pub text: &'a str,
}

/// A chunk's postings: each distinct token and its frequency, in token
/// order, with the reserved [`STATS_TOKEN`] carrying the chunk's length.
#[must_use]
pub fn postings(text: &str, config: &TokenizerConfig) -> BTreeMap<String, u64> {
    let tokens = tokenize(text, config);
    let mut counts: BTreeMap<String, u64> = BTreeMap::new();
    let length = u64::try_from(tokens.len()).unwrap_or(u64::MAX);
    for token in tokens {
        let count = counts.entry(token).or_insert(0);
        *count = count.saturating_add(1);
    }
    counts.insert(STATS_TOKEN.to_owned(), length);
    counts
}

/// Stage `chunk`'s postings and its `token_df` contribution into `txn`.
///
/// Call it after the chunk's previous postings, if any, were released
/// (`aicortex_store::text_postings`). The postings travel as one JSON
/// object per statement and are expanded by SQLite's built-in `json_each`,
/// so a chunk costs two statements of bounded size whatever its vocabulary
/// (D-10).
pub fn stage_chunk(txn: &mut TxnBuilder, chunk: &ChunkText<'_>, config: &TokenizerConfig) {
    let object = json_object(&postings(chunk.text, config));
    txn.push(Statement::with_params(
        "INSERT INTO chunk_token (chunk_id, scope_id, memory_id, model_revision, token, tf)
         SELECT $1, $2, $3, $4, j.key, j.value FROM json_each($5) AS j",
        vec![
            Value::from(chunk.chunk_id),
            Value::from(chunk.scope_id),
            Value::from(chunk.memory_id.to_string()),
            Value::Integer(i64::from(chunk.revision)),
            Value::from(object.as_str()),
        ],
    ));
    txn.push(Statement::with_params(
        "INSERT INTO token_df (scope_id, model_revision, token, df, cf)
         SELECT $1, $2, j.key, 1, j.value FROM json_each($3) AS j WHERE true
         ON CONFLICT (scope_id, model_revision, token)
         DO UPDATE SET df = df + 1, cf = cf + excluded.cf",
        vec![
            Value::from(chunk.scope_id),
            Value::Integer(i64::from(chunk.revision)),
            Value::from(object),
        ],
    ));
}

/// A JSON object of token to count, with tokens escaped as JSON strings.
fn json_object(map: &BTreeMap<String, u64>) -> String {
    let mut out = String::from("{");
    for (index, (token, tf)) in map.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push('"');
        for c in token.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                c if u32::from(c) < 0x20 => out.push_str(&format!("\\u{:04x}", u32::from(c))),
                c => out.push(c),
            }
        }
        out.push_str("\":");
        out.push_str(&tf.to_string());
    }
    out.push('}');
    out
}

/// One text search request.
#[derive(Clone, Copy, Debug)]
pub struct TextQuery<'a> {
    /// The scope searched; no posting of another scope is ever read.
    pub scope: &'a ScopeId,
    /// The model revision whose chunks are searched.
    pub revision: u32,
    /// The query text, tokenized with `tokenizer`.
    pub text: &'a str,
    /// How many candidates to return at most.
    pub k: usize,
    /// The memory statuses admitted.
    pub filter: &'a Filter,
    /// The tokenizer the postings were written with.
    pub tokenizer: TokenizerConfig,
}

#[derive(Debug, Deserialize)]
struct FrequencyRow {
    token: String,
    df: i64,
    cf: i64,
}

#[derive(Debug, Deserialize)]
struct PostingRow {
    chunk_id: String,
    memory_id: String,
    ordinal: i64,
    token: String,
    tf: i64,
    dl: i64,
}

/// The collection statistics BM25 needs, for one scope and revision.
#[derive(Clone, Debug, PartialEq)]
pub struct Statistics {
    /// Indexed chunks.
    pub chunks: u64,
    /// Average chunk length in tokens.
    pub average_length: f64,
    /// Document frequency of each query token present in the index.
    pub df: BTreeMap<String, u64>,
}

/// BM25 inverse document frequency, in the non-negative form
/// `ln(1 + (N - df + 0.5) / (df + 0.5))`.
#[must_use]
pub fn idf(chunks: u64, df: u64) -> f64 {
    let n = chunks as f64;
    let df = df as f64;
    (1.0 + (n - df + 0.5) / (df + 0.5)).ln()
}

/// One term's BM25 contribution for a chunk of length `dl`.
#[must_use]
pub fn term_score(idf: f64, tf: u64, dl: u64, average_length: f64) -> f64 {
    let tf = tf as f64;
    let norm = if average_length > 0.0 {
        dl as f64 / average_length
    } else {
        1.0
    };
    idf * tf * (BM25_K1 + 1.0) / (tf + BM25_K1 * (1.0 - BM25_B + BM25_B * norm))
}

fn count(value: i64, what: &str) -> Result<u64, Error> {
    u64::try_from(value).map_err(|_| Error::Integrity(format!("{what} {value} is negative")))
}

/// The statistics for `tokens` in one scope and revision.
async fn statistics(
    store: &StoreHandle,
    scope: &ScopeId,
    revision: u32,
    tokens: &[String],
) -> Result<Statistics, Error> {
    let placeholders = (0..=tokens.len())
        .map(|offset| format!("${}", offset + 3))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT token, df, cf FROM token_df
         WHERE scope_id = $1 AND model_revision = $2 AND token IN ({placeholders})"
    );
    let mut params = vec![
        Value::from(scope.as_str()),
        Value::Integer(i64::from(revision)),
        Value::from(STATS_TOKEN),
    ];
    params.extend(tokens.iter().map(|token| Value::from(token.as_str())));
    let rows: Vec<FrequencyRow> = store.query(sql, params).await?;
    let mut stats = Statistics {
        chunks: 0,
        average_length: 0.0,
        df: BTreeMap::new(),
    };
    for row in rows {
        if row.token == STATS_TOKEN {
            stats.chunks = count(row.df, "indexed chunk count")?;
            let total = count(row.cf, "indexed token count")?;
            if stats.chunks > 0 {
                stats.average_length = total as f64 / stats.chunks as f64;
            }
        } else {
            stats
                .df
                .insert(row.token, count(row.df, "document frequency")?);
        }
    }
    Ok(stats)
}

/// Search one scope's text index with BM25 (B-5).
///
/// Postings are read a page at a time in `(token, chunk_id)` order, so each
/// chunk accumulates its terms in token order and the sum is the same on
/// every run (B-4). The budget applies as it does to the vector scan (B-8).
///
/// # Errors
///
/// [`Error::Validation`] for a zero page size; store errors;
/// [`Error::Integrity`] for a negative stored count or a malformed id.
pub async fn search(
    store: &StoreHandle,
    query: &TextQuery<'_>,
    config: ScanConfig,
) -> Result<ScanOutcome, Error> {
    let started = Instant::now();
    if config.page_size == 0 {
        return Err(Error::Validation(
            "a search page of zero rows never advances".to_owned(),
        ));
    }
    let mut outcome = ScanOutcome {
        candidates: Vec::new(),
        truncated: false,
        scanned: 0,
        pages: 0,
    };
    let mut tokens = tokenize(query.text, &query.tokenizer);
    tokens.sort_unstable();
    tokens.dedup();
    if tokens.is_empty() || query.k == 0 {
        return Ok(outcome);
    }
    let stats = statistics(store, query.scope, query.revision, &tokens).await?;
    // Only tokens the index holds can match anything.
    let tokens: Vec<String> = tokens
        .into_iter()
        .filter(|token| stats.df.contains_key(token))
        .collect();
    if tokens.is_empty() || stats.chunks == 0 {
        return Ok(outcome);
    }

    let labels = query.filter.labels();
    let token_params = (0..tokens.len())
        .map(|offset| format!("?{}", offset + 5))
        .collect::<Vec<_>>()
        .join(", ");
    let status_params = (0..labels.len())
        .map(|offset| format!("?{}", offset + 5 + tokens.len()))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT p.chunk_id AS chunk_id, p.memory_id AS memory_id, c.ordinal AS ordinal,
                p.token AS token, p.tf AS tf, d.tf AS dl
         FROM chunk_token p
         JOIN chunk_token d ON d.chunk_id = p.chunk_id AND d.token = ''
         JOIN chunk c ON c.chunk_id = p.chunk_id
         JOIN memory m ON m.id = p.memory_id AND m.scope_id = p.scope_id
         WHERE p.scope_id = ?1 AND p.model_revision = ?2
           AND (p.token > ?3 OR (p.token = ?3 AND p.chunk_id > ?4))
           AND p.token IN ({token_params})
           AND m.status IN ({status_params})
         ORDER BY p.token, p.chunk_id"
    );
    let mut params = vec![
        Value::from(query.scope.as_str()),
        Value::Integer(i64::from(query.revision)),
        Value::Null,
        Value::Null,
    ];
    params.extend(tokens.iter().map(|token| Value::from(token.as_str())));
    params.extend(labels.iter().map(|label| Value::from(*label)));

    let idfs: HashMap<&str, f64> = stats
        .df
        .iter()
        .map(|(token, df)| (token.as_str(), idf(stats.chunks, *df)))
        .collect();
    // chunk id -> (memory id, ordinal, score so far)
    let mut scores: HashMap<String, (String, i64, f64)> = HashMap::new();
    let mut after = (String::new(), String::new());
    loop {
        if let Some(slot) = params.get_mut(2) {
            *slot = Value::from(after.0.as_str());
        }
        if let Some(slot) = params.get_mut(3) {
            *slot = Value::from(after.1.as_str());
        }
        let page: Vec<PostingRow> = store
            .query_paged(&sql, params.clone(), Page::new(config.page_size))
            .await?;
        outcome.pages = outcome.pages.saturating_add(1);
        let full = u32::try_from(page.len()).is_ok_and(|len| len >= config.page_size);
        if let Some(last) = page.last() {
            after = (last.token.clone(), last.chunk_id.clone());
        }
        for row in page {
            outcome.scanned = outcome.scanned.saturating_add(1);
            let weight = idfs.get(row.token.as_str()).copied().unwrap_or(0.0);
            let contribution = term_score(
                weight,
                count(row.tf, "term frequency")?,
                count(row.dl, "chunk length")?,
                stats.average_length,
            );
            let entry = scores
                .entry(row.chunk_id)
                .or_insert_with(|| (row.memory_id, row.ordinal, 0.0));
            entry.2 += contribution;
        }
        if !full {
            break;
        }
        if started.elapsed() >= config.budget {
            outcome.truncated = true;
            break;
        }
    }

    let mut top = TopK::new(query.k);
    for (chunk_id, (memory, ordinal, score)) in scores {
        top.offer(score, chunk_id, (memory, ordinal));
    }
    outcome.candidates = top
        .into_sorted()
        .into_iter()
        .map(|(score, chunk_id, (memory, ordinal))| {
            Ok(Candidate {
                chunk_id,
                memory_id: memory.parse::<MemoryId>().map_err(|error| {
                    Error::Integrity(format!("stored memory id is invalid: {error}"))
                })?,
                ordinal: u32::try_from(ordinal).map_err(|_| {
                    Error::Integrity(format!("chunk ordinal {ordinal} is outside u32"))
                })?,
                score,
            })
        })
        .collect::<Result<_, Error>>()?;
    Ok(outcome)
}
