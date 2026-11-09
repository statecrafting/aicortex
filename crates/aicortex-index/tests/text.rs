//! The text channel (spec 016 B-5, B-6, FR-004, FR-005).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]
// Spec 010 B-1: a crate root of aicortex-index, which carries the one
// documented float allow; the BM25 reference is floating point.
#![allow(clippy::float_arithmetic)]

mod common;

use std::collections::BTreeMap;

use aicortex_index::text::{ChunkText, TextQuery, idf, postings, search, stage_chunk, term_score};
use aicortex_index::tokenize::{Language, TOKENIZER_VERSION, TokenizerConfig, tokenize};
use aicortex_index::{Filter, ScanConfig};
use aicortex_store::ScopeId;
use aicortex_store::text_postings::{
    stage_delete_memory_postings, stage_drop_revision, stage_uncount_memory,
};
use aicortex_types::{MemoryId, Status};
use common::Node;
use rahi_store::{Statement, StoreHandle, TxnBuilder, Value};
use serde::Deserialize;

// ------------------------------------------------------------ tokenization

/// FR-005's committed fixtures. Changing the tokenizer so that any of these
/// changes is a migration of every stored posting: bump
/// `TOKENIZER_VERSION`, write the migration, and only then these lines.
const FIXTURES: &[(&str, &[&str])] = &[
    // CJK: ideographs and kana one per token; Hangul words stay whole.
    (
        "東京タワーに行きました",
        &[
            "東", "京", "タ", "ワ", "ー", "に", "行", "き", "ま", "し", "た",
        ],
    ),
    ("안녕하세요 세계", &["안녕하세요", "세계"]),
    ("数据库 database", &["数", "据", "库", "database"]),
    // Accented Latin keeps its letters and is lowercased, not folded.
    ("Café Ångström NAÏVE", &["café", "ångström", "naïve"]),
    // NFKC brings full-width forms to their ordinary spelling.
    ("ｆｕｌｌ－ｗｉｄｔｈ", &["full", "width"]),
    // Hyphenated compounds split into their parts; stop words drop.
    ("state-of-the-art e-mail", &["state", "art", "e", "mail"]),
    // Code identifiers: whole, then camel-case parts; snake_case splits.
    (
        "parseHttpRequest snake_case_name XMLParser utf8",
        &[
            "parsehttprequest",
            "parse",
            "http",
            "request",
            "snake",
            "case",
            "name",
            "xmlparser",
            "xml",
            "parser",
            "utf8",
        ],
    ),
    ("aicortex::index::scan()", &["aicortex", "index", "scan"]),
    // Contractions and possessives; the light plural stemmer.
    ("Bart's notes don't", &["bart", "note", "dont"]),
    (
        "queries policies buses glass",
        &["query", "policy", "buse", "glass"],
    ),
    // Combining marks stay with their letter (Devanagari).
    ("नमस्ते दुनिया", &["नमस्ते", "दुनिया"]),
];

#[test]
fn fr005_tokenizer_fixtures_hold() {
    assert_eq!(TOKENIZER_VERSION, 1);
    let config = TokenizerConfig::default();
    for (input, expected) in FIXTURES {
        assert_eq!(
            tokenize(input, &config),
            expected.iter().map(|t| (*t).to_owned()).collect::<Vec<_>>(),
            "tokenizing {input:?}"
        );
    }
}

#[test]
fn the_length_limit_stop_words_and_language_apply() {
    let config = TokenizerConfig::default();
    let long = format!("{} ok", "x".repeat(65));
    assert_eq!(tokenize(&long, &config), vec!["ok"]);
    assert_eq!(tokenize("The cats", &config), vec!["cat"]);
    let neutral = TokenizerConfig {
        language: Language::Neutral,
        ..config
    };
    assert_eq!(tokenize("The cats", &neutral), vec!["the", "cats"]);
    let unstemmed = TokenizerConfig {
        stem: false,
        ..config
    };
    assert_eq!(tokenize("The cats", &unstemmed), vec!["cats"]);
    assert!(tokenize("  --  ", &config).is_empty());
}

#[test]
fn the_text_index_migration_follows_the_embedding_schema_and_is_not_additive() {
    let migration = aicortex_index::migrations::migration();
    assert_eq!(
        migration.version,
        aicortex_index::migrations::TEXT_INDEX_VERSION
    );
    assert_eq!(migration.version, aicortex_store::EXPECTED_SCHEMA_VERSION);
    assert!(migration.version > aicortex_store::EMBEDDING_MIGRATION_VERSION);
    assert!(migration.sql.contains("CREATE TABLE chunk_token"));
    assert!(migration.sql.contains("CREATE TABLE token_df"));
    assert!(!migration.additive);
}

#[test]
fn postings_carry_the_chunk_length_under_the_reserved_token() {
    let counts = postings("cats chase cats", &TokenizerConfig::default());
    assert_eq!(counts.get(""), Some(&3));
    assert_eq!(counts.get("cat"), Some(&2));
    assert_eq!(counts.get("chase"), Some(&1));
}

// ------------------------------------------------------------------- BM25

/// FR-004's committed corpus: one chunk per entry.
const CORPUS: &[&str] = &[
    "The embedding worker drains the outbox and retries with backoff.",
    "Erasure removes chunks, embeddings, and index entries in one transaction.",
    "A scan reads vectors page by page and keeps a bounded heap.",
    "BM25 scores postings with term frequency and document frequency.",
    "The outbox stages work in the capture transaction; the worker retries.",
    "Quarantined memories are invisible to retrieval by default.",
    "Index entries are postings: a token, a chunk, and a frequency.",
    "Retrieval quality is measured against an evaluation corpus.",
    "worker worker worker outbox",
];

const QUERIES: &[&str] = &[
    "worker retries outbox",
    "index entries postings",
    "transaction",
    "retrieval corpus quality",
    "nothing matches this",
];

fn chunk_name(index: usize) -> String {
    format!("text-chunk-{index:03}")
}

/// The reference: BM25 computed directly from token lists, k1 = 1.2 and
/// b = 0.75, idf = ln(1 + (N - df + 0.5) / (df + 0.5)).
fn reference(query: &str, k: usize) -> Vec<(usize, f64)> {
    let config = TokenizerConfig::default();
    let docs: Vec<Vec<String>> = CORPUS.iter().map(|d| tokenize(d, &config)).collect();
    let n = docs.len() as f64;
    let total: usize = docs.iter().map(Vec::len).sum();
    let average = total as f64 / n;
    let mut terms = tokenize(query, &config);
    terms.sort();
    terms.dedup();
    let mut scored = Vec::new();
    for (index, doc) in docs.iter().enumerate() {
        let mut score = 0.0;
        let mut matched = false;
        for term in &terms {
            let df = docs.iter().filter(|d| d.contains(term)).count() as f64;
            let tf = doc.iter().filter(|t| *t == term).count() as f64;
            if df == 0.0 || tf == 0.0 {
                continue;
            }
            matched = true;
            let w = ((n - df + 0.5) / (df + 0.5) + 1.0).ln();
            let len = doc.len() as f64;
            score += w * (tf * 2.2) / (tf + 1.2 * (0.25 + 0.75 * len / average));
        }
        if matched {
            scored.push((index, score));
        }
    }
    scored.sort_by(|a, b| {
        b.1.total_cmp(&a.1)
            .then_with(|| chunk_name(a.0).cmp(&chunk_name(b.0)))
    });
    scored.truncate(k);
    scored
}

/// Write `texts` as one memory each, one chunk each, with their postings.
async fn index_texts(
    store: &StoreHandle,
    scope_id: &ScopeId,
    revision: u32,
    first_memory: u64,
    texts: &[&str],
    status: Status,
    prefix: &str,
) -> Vec<(MemoryId, String)> {
    let mut txn = TxnBuilder::new();
    let mut written = Vec::new();
    for (index, text) in texts.iter().enumerate() {
        let memory = common::memory_id(first_memory + index as u64);
        let chunk_id = format!("{prefix}-{index:03}");
        txn.push(Statement::with_params(
            "INSERT INTO memory
             (id, scope_id, status, kind, trust, fingerprint, schema_version, body_bytes,
              created, updated, record)
             VALUES ($1, $2, $3, 'note', 'first_party', $1, 1, 1, 1700000000, 1700000000, '{}')
             ON CONFLICT (id) DO NOTHING",
            vec![
                Value::from(memory.to_string()),
                Value::from(scope_id.as_str()),
                Value::from(status.label()),
            ],
        ));
        txn.push(Statement::with_params(
            "INSERT INTO chunk
             (chunk_id, scope_id, memory_id, model_revision, ordinal, byte_start, byte_end)
             VALUES ($1, $2, $3, $4, 0, 0, 1)",
            vec![
                Value::from(chunk_id.as_str()),
                Value::from(scope_id.as_str()),
                Value::from(memory.to_string()),
                Value::Integer(i64::from(revision)),
            ],
        ));
        stage_chunk(
            &mut txn,
            &ChunkText {
                scope_id: scope_id.as_str(),
                memory_id: memory,
                revision,
                chunk_id: &chunk_id,
                text,
            },
            &TokenizerConfig::default(),
        );
        written.push((memory, chunk_id));
    }
    store.txn(txn.into_statements()).await.unwrap();
    written
}

async fn text_node(name: &str) -> (Node, ScopeId) {
    let node = Node::migrated().await;
    let store = node.handle();
    let (scope, scope_id) = common::scope(name);
    common::register_scope(&store, &scope).await;
    common::register_model(&store, &common::revision(1, 8)).await;
    (node, scope_id)
}

fn query<'a>(scope: &'a ScopeId, text: &'a str, filter: &'a Filter) -> TextQuery<'a> {
    TextQuery {
        scope,
        revision: 1,
        text,
        k: 5,
        filter,
        tokenizer: TokenizerConfig::default(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fr004_bm25_matches_the_reference_on_the_committed_corpus() {
    let (node, scope_id) = text_node("bm25").await;
    let store = node.handle();
    index_texts(
        &store,
        &scope_id,
        1,
        1,
        CORPUS,
        Status::Active,
        "text-chunk",
    )
    .await;

    let filter = Filter::retrievable();
    for text in QUERIES {
        let expected = reference(text, 5);
        for page_size in [1, 3, 1024] {
            let found = search(
                &store,
                &query(&scope_id, text, &filter),
                ScanConfig {
                    page_size,
                    ..ScanConfig::default()
                },
            )
            .await
            .unwrap();
            assert!(!found.truncated);
            assert_eq!(
                found.candidates.len(),
                expected.len(),
                "{text:?} at page size {page_size}"
            );
            for (candidate, (index, score)) in found.candidates.iter().zip(&expected) {
                assert_eq!(candidate.chunk_id, chunk_name(*index), "{text:?}");
                assert!(
                    (candidate.score - score).abs() <= 1e-6,
                    "{text:?}: {} vs reference {score}",
                    candidate.score
                );
            }
        }
    }
    assert!(reference("worker retries outbox", 5).len() >= 3);
    // The building blocks are the formula the reference spells out.
    assert!((idf(10, 10) - (1.0_f64 + 0.5 / 10.5).ln()).abs() < 1e-12);
    assert!((term_score(1.0, 1, 4, 4.0) - 1.0).abs() < 1e-12);
    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn text_search_stays_in_scope_and_honours_the_status_filter() {
    let (node, scope_a) = text_node("text-a").await;
    let store = node.handle();
    let (scope_b_value, scope_b) = common::scope("text-b");
    common::register_scope(&store, &scope_b_value).await;
    let shared = ["shared words here"];
    index_texts(&store, &scope_a, 1, 1, &shared, Status::Active, "a").await;
    let there = ["shared words there"];
    index_texts(&store, &scope_b, 1, 100, &there, Status::Active, "b").await;
    let held = ["shared quarantine"];
    index_texts(&store, &scope_a, 1, 200, &held, Status::Quarantined, "q").await;

    let filter = Filter::retrievable();
    let found = search(
        &store,
        &query(&scope_a, "shared", &filter),
        ScanConfig::default(),
    )
    .await
    .unwrap();
    let ids: Vec<&str> = found
        .candidates
        .iter()
        .map(|c| c.chunk_id.as_str())
        .collect();
    assert_eq!(ids, vec!["a-000"]);

    let widened = Filter::statuses([Status::Active, Status::Quarantined]).unwrap();
    let found = search(
        &store,
        &query(&scope_a, "shared", &widened),
        ScanConfig::default(),
    )
    .await
    .unwrap();
    assert_eq!(found.candidates.len(), 2);

    let other_revision = TextQuery {
        revision: 2,
        ..query(&scope_a, "shared", &filter)
    };
    let found = search(&store, &other_revision, ScanConfig::default())
        .await
        .unwrap();
    assert!(found.candidates.is_empty());
    node.shutdown().await;
}

#[derive(Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
struct Frequency {
    model_revision: i64,
    token: String,
    df: i64,
    cf: i64,
}

/// `token_df` as stored, and as recomputed from the postings.
async fn frequencies(store: &StoreHandle, scope_id: &ScopeId) -> (Vec<Frequency>, Vec<Frequency>) {
    let stored: Vec<Frequency> = store
        .query_consistent(
            "SELECT model_revision, token, df, cf FROM token_df WHERE scope_id = $1
             ORDER BY model_revision, token",
            vec![Value::from(scope_id.as_str())],
        )
        .await
        .unwrap();
    let derived: Vec<Frequency> = store
        .query_consistent(
            "SELECT model_revision, token, COUNT(*) AS df, SUM(tf) AS cf FROM chunk_token
             WHERE scope_id = $1 GROUP BY model_revision, token
             ORDER BY model_revision, token",
            vec![Value::from(scope_id.as_str())],
        )
        .await
        .unwrap();
    (stored, derived)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn releasing_postings_keeps_token_df_exact_and_forgets_erased_tokens() {
    let (node, scope_id) = text_node("release").await;
    let store = node.handle();
    common::register_model(&store, &common::revision(2, 8)).await;
    let written = index_texts(&store, &scope_id, 1, 1, CORPUS, Status::Active, "r1").await;
    index_texts(&store, &scope_id, 2, 1, CORPUS, Status::Active, "r2").await;
    let (stored, derived) = frequencies(&store, &scope_id).await;
    assert_eq!(stored, derived);
    let stats: BTreeMap<_, _> = stored
        .iter()
        .filter(|f| f.token.is_empty())
        .map(|f| (f.model_revision, f.df))
        .collect();
    assert_eq!(stats.get(&1), Some(&(CORPUS.len() as i64)));

    // Re-embedding one memory at revision 1: release, then index anew.
    let (memory, chunk_id) = &written[8];
    let mut txn = TxnBuilder::new();
    stage_uncount_memory(&mut txn, scope_id.as_str(), &memory.to_string(), Some(1));
    stage_delete_memory_postings(&mut txn, scope_id.as_str(), &memory.to_string(), Some(1));
    stage_chunk(
        &mut txn,
        &ChunkText {
            scope_id: scope_id.as_str(),
            memory_id: *memory,
            revision: 1,
            chunk_id,
            text: "a replacement about zeppelins",
        },
        &TokenizerConfig::default(),
    );
    store.txn(txn.into_statements()).await.unwrap();
    let (stored, derived) = frequencies(&store, &scope_id).await;
    assert_eq!(stored, derived);
    assert!(stored.iter().any(|f| f.token == "zeppelin"));

    // Erasing a memory at every revision forgets the tokens only it held.
    let mut txn = TxnBuilder::new();
    stage_uncount_memory(&mut txn, scope_id.as_str(), &memory.to_string(), None);
    stage_delete_memory_postings(&mut txn, scope_id.as_str(), &memory.to_string(), None);
    store.txn(txn.into_statements()).await.unwrap();
    let (stored, derived) = frequencies(&store, &scope_id).await;
    assert_eq!(stored, derived);
    assert!(!stored.iter().any(|f| f.token == "zeppelin"));

    // Erasing everything leaves nothing behind, not even the statistics.
    let mut txn = TxnBuilder::new();
    for (memory, _) in &written {
        stage_uncount_memory(&mut txn, scope_id.as_str(), &memory.to_string(), None);
        stage_delete_memory_postings(&mut txn, scope_id.as_str(), &memory.to_string(), None);
    }
    store.txn(txn.into_statements()).await.unwrap();
    let (stored, derived) = frequencies(&store, &scope_id).await;
    assert!(stored.is_empty(), "{stored:?}");
    assert!(derived.is_empty());
    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropping_an_inactive_revision_drops_its_postings() {
    let (node, scope_id) = text_node("drop").await;
    let store = node.handle();
    common::register_model(&store, &common::revision(2, 8)).await;
    index_texts(&store, &scope_id, 1, 1, CORPUS, Status::Active, "d1").await;
    index_texts(&store, &scope_id, 2, 1, CORPUS, Status::Active, "d2").await;
    store
        .execute(
            "UPDATE embedding_model SET active = CASE revision WHEN 2 THEN 1 ELSE 0 END",
            vec![],
        )
        .await
        .unwrap();
    let mut txn = TxnBuilder::new();
    stage_drop_revision(&mut txn, scope_id.as_str(), 2);
    store.txn(txn.into_statements()).await.unwrap();
    let (stored, _) = frequencies(&store, &scope_id).await;
    assert!(
        stored.iter().any(|f| f.model_revision == 2),
        "2 is active and stays"
    );

    let mut txn = TxnBuilder::new();
    stage_drop_revision(&mut txn, scope_id.as_str(), 1);
    store.txn(txn.into_statements()).await.unwrap();
    let (stored, derived) = frequencies(&store, &scope_id).await;
    assert_eq!(stored, derived);
    assert!(stored.iter().all(|f| f.model_revision == 2));
    node.shutdown().await;
}
