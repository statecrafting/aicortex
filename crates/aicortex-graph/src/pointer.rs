//! Entity pointers: which known entities a text names (spec 026 B-1 to B-4,
//! B-6, B-7).
//!
//! [`pointers`] answers "does the store already hold something about anyone
//! this text names". It resolves names by 017 B-2's first two steps only:
//! an alias equal to the text's own span, then an alias with the same
//! normalized form. It never compares embeddings, never calls a model, never
//! creates an entity, and never records a mention. It takes a [`ReadStore`],
//! so it cannot write, and it persists, caches, and logs nothing: the result
//! is a pure function of the stored rows and the arguments.
//!
//! Every statement binds the scope it was asked about as `?1` in the
//! predicate of every table it reads, so no row of another scope is read
//! (B-7). An entity is pointed at only while an `active` memory in the scope
//! mentions it, and its count and newest time are computed over `active`
//! memories only, so a pointer never reveals that erased, quarantined,
//! superseded, or expired content existed (B-3).
//!
//! A pointer holds an entity id, its kind, its canonical name, a count, a
//! time, and the byte range of the match. The canonical name is derived from
//! stored content, so this crate hands it to a surface as data; the surface
//! frames it through 019 (B-5) and this crate serializes nothing.
//!
//! Where a text is searched for names. A name is a run of one to
//! [`MAX_NAME_WORDS`] words, where a word is a maximal run of alphanumeric
//! characters and combining marks (the characters 017's [`normalize`] keeps).
//! A name's span runs from the start of its first word to the end of its
//! last, so an alias with punctuation after its last word (`Dana W.`) is found
//! by its normalized form. Two names that overlap are not both pointed at: the scan runs left to
//! right and takes the longest name that resolves at each position, so
//! "Dana Whitfield" is one pointer, not "Dana" and "Whitfield".

use core::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};

use aicortex_store::ReadStore;
use aicortex_types::MemoryId;
use rahi_store::Value;
use rahi_types::{Error, UnixSeconds};
use serde::Deserialize;
use unicode_normalization::char::is_combining_mark;

use crate::entity::{EntityId, normalize};
use crate::vocabulary::EntityKind;

/// The most pointers a set holds when a deployment names no bound (B-6).
pub const DEFAULT_MAX_POINTERS: usize = 5;

/// The most pointers any configuration may ask for.
pub const MAX_POINTERS: usize = 50;

/// The bytes of text resolved over when a deployment names no bound (B-6):
/// 8 KiB.
pub const DEFAULT_TEXT_BYTES: usize = 8 * 1024;

/// The most bytes of text any configuration may resolve over: 64 KiB.
pub const MAX_TEXT_BYTES: usize = 64 * 1024;

/// The most words one name may span.
///
/// A longer alias is never matched. The bound keeps the candidate lookups
/// linear in the text: each word starts at most this many names.
pub const MAX_NAME_WORDS: usize = 6;

/// Names looked up per statement; the lookups of a longer text are batches.
const BATCH: usize = 256;

/// The most alias rows one lookup statement returns. A batch of [`BATCH`]
/// names would need more than sixteen entities sharing each name to reach it.
const MAX_ALIAS_ROWS: i64 = 4096;

/// What bounds one resolution (B-6).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PointerBounds {
    max_pointers: usize,
    text_bytes: usize,
}

impl PointerBounds {
    /// Bounds of at most `max_pointers` pointers over the first `text_bytes`
    /// bytes of a text. Each is clamped into its range: 1 to
    /// [`MAX_POINTERS`], and 1 to [`MAX_TEXT_BYTES`].
    #[must_use]
    pub fn new(max_pointers: usize, text_bytes: usize) -> Self {
        Self {
            max_pointers: max_pointers.clamp(1, MAX_POINTERS),
            text_bytes: text_bytes.clamp(1, MAX_TEXT_BYTES),
        }
    }

    /// The most pointers a set holds.
    #[must_use]
    pub const fn max_pointers(&self) -> usize {
        self.max_pointers
    }

    /// The bytes of text resolved over.
    #[must_use]
    pub const fn text_bytes(&self) -> usize {
        self.text_bytes
    }
}

impl Default for PointerBounds {
    fn default() -> Self {
        Self {
            max_pointers: DEFAULT_MAX_POINTERS,
            text_bytes: DEFAULT_TEXT_BYTES,
        }
    }
}

/// One entity a text names (B-4).
///
/// It holds no memory body, no edge object, no alias list, and no sentence
/// derived from stored text. `canonical` is derived from stored content and
/// must leave through 019's framing. `trust` and `newest` name the provenance
/// of the pointer, as 056's page names a fact's: the strongest trust class
/// among the entity's `active` mentioning memories, and the newest of them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pointer {
    /// The entity's identity: the survivor's, when it was merged (017 B-2).
    pub entity: EntityId,
    /// What it is (017 B-1).
    pub kind: EntityKind,
    /// The name it is shown under.
    pub canonical: String,
    /// How many `active` memories mention it.
    pub memories: u64,
    /// When the newest of those was captured.
    pub last_seen: UnixSeconds,
    /// The byte offset in the resolved text where the match starts.
    pub start: u64,
    /// The byte offset where the match ends, exclusive.
    pub end: u64,
    /// The strongest trust class label among those memories.
    pub trust: &'static str,
    /// The newest of those memories.
    pub newest: MemoryId,
}

/// The pointers a text resolved to (B-6).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PointerSet {
    /// The pointers, by first match position, then mention count descending,
    /// then entity id.
    pub pointers: Vec<Pointer>,
    /// Whether more entities resolved than the bound allows and the rest were
    /// cut from the end.
    pub truncated: bool,
    /// Whether the text was longer than the bound and only its prefix was
    /// resolved over. Independent of `truncated`.
    pub text_truncated: bool,
}

/// One word of the resolved text: its byte span in the text and its
/// normalized form.
struct Word {
    start: usize,
    end: usize,
    form: String,
}

/// The longest prefix of `text` within `limit` bytes that ends on a character
/// boundary.
fn prefix(text: &str, limit: usize) -> &str {
    let mut end = limit.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.get(..end).unwrap_or_default()
}

fn words(text: &str) -> Vec<Word> {
    let mut out = Vec::new();
    let mut open: Option<usize> = None;
    let close = |start: usize, end: usize, out: &mut Vec<Word>| {
        let form = text.get(start..end).map(normalize).unwrap_or_default();
        if !form.is_empty() {
            out.push(Word { start, end, form });
        }
    };
    for (at, c) in text.char_indices() {
        let part = c.is_alphanumeric() || is_combining_mark(c);
        match (open, part) {
            (None, true) => open = Some(at),
            (Some(start), false) => {
                close(start, at, &mut out);
                open = None;
            }
            _ => {}
        }
    }
    if let Some(start) = open {
        close(start, text.len(), &mut out);
    }
    out
}

/// The span names: for each word, the names of one to [`MAX_NAME_WORDS`]
/// words that start there, as `(first word, last word)` indices.
fn spans(count: usize) -> impl Iterator<Item = (usize, usize)> {
    (0..count).flat_map(move |first| {
        (first..count.min(first + MAX_NAME_WORDS)).map(move |last| (first, last))
    })
}

fn surface<'a>(text: &'a str, words: &[Word], first: usize, last: usize) -> Option<&'a str> {
    let start = words.get(first)?.start;
    let end = words.get(last)?.end;
    text.get(start..end)
}

fn form(words: &[Word], first: usize, last: usize) -> Option<String> {
    let part = words.get(first..=last)?;
    Some(
        part.iter()
            .map(|word| word.form.as_str())
            .collect::<Vec<_>>()
            .join(" "),
    )
}

/// An alias match: the name matched, the entity it resolves to (the survivor
/// when the owner was merged), and the owner's first-seen time, which orders
/// two entities sharing a name as 017 B-2's resolution does.
#[derive(Debug, Deserialize)]
struct Hit {
    name: String,
    entity_id: String,
    first_seen: i64,
}

const EXACT_SQL: &str = "SELECT a.alias AS name,
            COALESCE(e.merged_into, e.entity_id) AS entity_id,
            e.first_seen AS first_seen
     FROM entity_alias a JOIN entity e
       ON e.scope_id = a.scope_id AND e.entity_id = a.entity_id
     WHERE a.scope_id = ?1 AND a.alias IN (SELECT value FROM json_each(?2))
     ORDER BY a.alias, e.first_seen, e.entity_id LIMIT ?3";

const NORMALIZED_SQL: &str = "SELECT a.normalized AS name,
            COALESCE(e.merged_into, e.entity_id) AS entity_id,
            e.first_seen AS first_seen
     FROM entity_alias a JOIN entity e
       ON e.scope_id = a.scope_id AND e.entity_id = a.entity_id
     WHERE a.scope_id = ?1 AND a.normalized IN (SELECT value FROM json_each(?2))
     ORDER BY a.normalized, e.first_seen, e.entity_id LIMIT ?3";

/// What the live mentions of a batch of entities say (B-3, B-4).
///
/// `roots` is an entity and every entity merged into it. `live` is each root's
/// distinct `active` mentioning memories in this scope. An entity with none is
/// absent from the answer.
const LIVE_SQL: &str = "WITH roots AS (
       SELECT entity_id AS member, COALESCE(merged_into, entity_id) AS root
       FROM entity
       WHERE scope_id = ?1
         AND COALESCE(merged_into, entity_id) IN (SELECT value FROM json_each(?2))
     ), live AS (
       SELECT DISTINCT r.root AS root, m.id AS id, m.created AS created,
              CASE m.trust WHEN 'instruction' THEN 0 WHEN 'evidence' THEN 1 ELSE 2 END AS rank
       FROM roots r
       JOIN mention x ON x.scope_id = ?1 AND x.entity_id = r.member
       JOIN memory m ON m.scope_id = ?1 AND m.id = x.memory_id
       WHERE m.status = 'active'
     )
     SELECT e.entity_id AS entity_id, e.kind AS kind, e.canonical AS canonical,
            COUNT(*) AS memories, MAX(l.created) AS newest, MIN(l.rank) AS rank,
            (SELECT n.id FROM live n WHERE n.root = l.root
             ORDER BY n.created DESC, n.id LIMIT 1) AS newest_id
     FROM live l JOIN entity e ON e.scope_id = ?1 AND e.entity_id = l.root
     GROUP BY l.root, e.entity_id, e.kind, e.canonical
     ORDER BY e.entity_id";

#[derive(Debug, Deserialize)]
struct LiveRow {
    entity_id: String,
    kind: String,
    canonical: String,
    memories: i64,
    newest: i64,
    rank: i64,
    newest_id: String,
}

/// A live entity, as the statement above reports it.
struct Live {
    kind: EntityKind,
    canonical: String,
    memories: u64,
    last_seen: UnixSeconds,
    trust: &'static str,
    newest: MemoryId,
}

fn trust_of(rank: i64) -> Result<&'static str, Error> {
    match rank {
        0 => Ok("instruction"),
        1 => Ok("evidence"),
        2 => Ok("assertion"),
        other => Err(Error::Integrity(format!(
            "trust rank {other} is not one the pointer statement computes: 0, 1, or 2"
        ))),
    }
}

fn names_param(names: &[&String]) -> Result<Value, Error> {
    serde_json::to_string(names)
        .map(Value::from)
        .map_err(|error| Error::Integrity(format!("a name list did not encode: {error}")))
}

/// Alias matches for `names` in `scope_id`, by the statement `sql`, grouped by
/// the name they matched and ordered as resolution orders them.
async fn lookup(
    store: &impl ReadStore,
    sql: &'static str,
    scope_id: &str,
    names: &BTreeSet<String>,
) -> Result<BTreeMap<String, Vec<(i64, String)>>, Error> {
    let names: Vec<&String> = names.iter().collect();
    let mut found: BTreeMap<String, Vec<(i64, String)>> = BTreeMap::new();
    for batch in names.chunks(BATCH) {
        let rows: Vec<Hit> = store
            .query(
                sql,
                vec![
                    Value::from(scope_id),
                    names_param(batch)?,
                    Value::Integer(MAX_ALIAS_ROWS),
                ],
            )
            .await?;
        for hit in rows {
            found
                .entry(hit.name)
                .or_default()
                .push((hit.first_seen, hit.entity_id));
        }
    }
    for entities in found.values_mut() {
        entities.sort();
        entities.dedup();
    }
    Ok(found)
}

/// The live facts of `entities` in `scope_id`.
async fn live(
    store: &impl ReadStore,
    scope_id: &str,
    entities: &BTreeSet<String>,
) -> Result<BTreeMap<String, Live>, Error> {
    let ids: Vec<&String> = entities.iter().collect();
    let mut out = BTreeMap::new();
    for batch in ids.chunks(BATCH) {
        let rows: Vec<LiveRow> = store
            .query(LIVE_SQL, vec![Value::from(scope_id), names_param(batch)?])
            .await?;
        for row in rows {
            let kind = EntityKind::parse(&row.kind).ok_or_else(|| {
                Error::Integrity(format!(
                    "stored entity kind {:?} is not in the vocabulary",
                    row.kind
                ))
            })?;
            let newest = MemoryId::parse(&row.newest_id)?;
            out.insert(
                row.entity_id,
                Live {
                    kind,
                    canonical: row.canonical,
                    memories: u64::try_from(row.memories).unwrap_or(0),
                    last_seen: UnixSeconds::new(u64::try_from(row.newest).unwrap_or(0)),
                    trust: trust_of(row.rank)?,
                    newest,
                },
            );
        }
    }
    Ok(out)
}

/// The first of `matches` that is live, in the order the lookup gave.
fn first_live<'a>(
    matches: Option<&'a Vec<(i64, String)>>,
    live: &BTreeMap<String, Live>,
) -> Option<&'a str> {
    matches?
        .iter()
        .map(|(_, entity)| entity.as_str())
        .find(|entity| live.contains_key(*entity))
}

/// Resolve the entities `text` names in `scope_id` (B-1 to B-4, B-6, B-7).
///
/// Only the first [`PointerBounds::text_bytes`] bytes of `text`, cut at a
/// character boundary, are resolved over; a name that starts inside that
/// prefix and ends past it is not matched, and `text_truncated` says so.
/// More entities than [`PointerBounds::max_pointers`] are cut from the end
/// and `truncated` says so.
///
/// An entity that no `active` memory in the scope mentions is never pointed
/// at, and a name shared by two entities resolves to the live one first seen
/// earliest, as 017 B-2's resolution would.
///
/// # Errors
///
/// Store errors, a stored kind outside the vocabulary, or a stored memory id
/// that does not parse.
pub async fn pointers(
    store: &impl ReadStore,
    scope_id: &str,
    text: &str,
    bounds: &PointerBounds,
) -> Result<PointerSet, Error> {
    let text_truncated = text.len() > bounds.text_bytes;
    let text = prefix(text, bounds.text_bytes);
    let words = words(text);

    let mut exact = BTreeSet::new();
    let mut normalized = BTreeSet::new();
    for (first, last) in spans(words.len()) {
        exact.extend(surface(text, &words, first, last).map(str::to_owned));
        normalized.extend(form(&words, first, last));
    }
    let exact_hits = lookup(store, EXACT_SQL, scope_id, &exact).await?;
    let normalized_hits = lookup(store, NORMALIZED_SQL, scope_id, &normalized).await?;

    let candidates: BTreeSet<String> = exact_hits
        .values()
        .chain(normalized_hits.values())
        .flatten()
        .map(|(_, entity)| entity.clone())
        .collect();
    let live = live(store, scope_id, &candidates).await?;

    // Left to right, the longest name that resolves at each position; an
    // exact alias beats a normalized one for the same span.
    let mut matched: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
    let mut at = 0;
    while at < words.len() {
        let longest = words.len().min(at + MAX_NAME_WORDS);
        let found = (at..longest).rev().find_map(|last| {
            let by_exact = surface(text, &words, at, last)
                .and_then(|name| first_live(exact_hits.get(name), &live));
            let entity = by_exact.or_else(|| {
                let name = form(&words, at, last)?;
                first_live(normalized_hits.get(&name), &live)
            })?;
            Some((entity, last))
        });
        match found {
            Some((entity, last)) => {
                let start = words.get(at).map_or(0, |word| word.start);
                let end = words.get(last).map_or(0, |word| word.end);
                matched.entry(entity).or_insert((start, end));
                at = last + 1;
            }
            None => at += 1,
        }
    }

    let mut pointers = Vec::with_capacity(matched.len());
    for (entity, (start, end)) in matched {
        let Some(facts) = live.get(entity) else {
            continue;
        };
        pointers.push(Pointer {
            entity: EntityId::new(entity),
            kind: facts.kind,
            canonical: facts.canonical.clone(),
            memories: facts.memories,
            last_seen: facts.last_seen,
            start: start as u64,
            end: end as u64,
            trust: facts.trust,
            newest: facts.newest,
        });
    }
    pointers.sort_by(|a, b| {
        (a.start, Reverse(a.memories), &a.entity).cmp(&(b.start, Reverse(b.memories), &b.entity))
    });
    let truncated = pointers.len() > bounds.max_pointers;
    pointers.truncate(bounds.max_pointers);
    Ok(PointerSet {
        pointers,
        truncated,
        text_truncated,
    })
}

#[cfg(test)]
#[allow(clippy::indexing_slicing, clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn a_prefix_ends_on_a_character_boundary() {
        assert_eq!(prefix("abc", 10), "abc");
        assert_eq!(prefix("ab\u{e9}", 3), "ab");
        assert_eq!(prefix("\u{e9}", 0), "");
    }

    #[test]
    fn words_are_alphanumeric_runs_with_their_byte_spans() {
        let found = words("Dana W., dark-mode!");
        let spans: Vec<(usize, usize, &str)> = found
            .iter()
            .map(|word| (word.start, word.end, word.form.as_str()))
            .collect();
        assert_eq!(
            spans,
            vec![
                (0, 4, "dana"),
                (5, 6, "w"),
                (9, 13, "dark"),
                (14, 18, "mode")
            ]
        );
    }

    #[test]
    fn a_decomposed_accent_stays_inside_its_word() {
        let found = words("Jose\u{301} Smith");
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].form, normalize("Jos\u{e9}"));
    }

    #[test]
    fn a_name_spans_at_most_the_name_word_bound() {
        let longest = spans(10).map(|(first, last)| last - first + 1).max();
        assert_eq!(longest, Some(MAX_NAME_WORDS));
        assert_eq!(spans(0).count(), 0);
    }

    #[test]
    fn bounds_are_clamped_into_their_ranges() {
        let low = PointerBounds::new(0, 0);
        assert_eq!((low.max_pointers(), low.text_bytes()), (1, 1));
        let high = PointerBounds::new(usize::MAX, usize::MAX);
        assert_eq!(
            (high.max_pointers(), high.text_bytes()),
            (MAX_POINTERS, MAX_TEXT_BYTES)
        );
        assert_eq!(PointerBounds::default().max_pointers(), 5);
        assert_eq!(PointerBounds::default().text_bytes(), 8 * 1024);
    }
}
