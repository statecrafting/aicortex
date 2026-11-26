//! Extraction: proposing mentions and edges from a memory's text (spec 017
//! B-5, B-8, D-2).
//!
//! An [`Extractor`] only *proposes*. [`validate`] holds every proposal to the
//! closed vocabulary and the confidence floor, and an edge that fails is
//! rejected and counted, never stored as free text (B-5). The default
//! [`PatternExtractor`] is deterministic and local: a table of relation
//! phrases, an explicit `Subject --predicate--> Object` form, and a
//! gazetteer of the entities the scope already knows. A model-based
//! extractor is a configured alternative behind the same trait; it runs
//! through the governed egress facade and its output passes the same
//! `validate` (B-8).

use crate::entity::normalize;
use crate::vocabulary::{EntityKind, Predicate, Slot};

/// The lowest confidence an edge may carry and be stored, in permille.
pub const CONFIDENCE_FLOOR: u16 = 500;

/// The longest literal object the default extractor proposes, in bytes.
pub const MAX_LITERAL: usize = 200;

/// An entity the scope already holds, offered to the extractor as a
/// gazetteer entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KnownEntity {
    /// Its kind.
    pub kind: EntityKind,
    /// Its canonical name and aliases.
    pub surfaces: Vec<String>,
}

/// A span of the text that names an entity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MentionProposal {
    /// The text as written.
    pub surface: String,
    /// The kind proposed.
    pub kind: EntityKind,
    /// The first byte of the span.
    pub start: usize,
    /// One past the last byte of the span.
    pub end: usize,
    /// Confidence in permille.
    pub confidence: u16,
}

/// The object of a proposed edge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ObjectProposal {
    /// An entity named in the text.
    Entity {
        /// The text as written.
        surface: String,
        /// The kind proposed.
        kind: EntityKind,
    },
    /// Literal text.
    Literal(String),
}

/// An edge as an extractor proposes it: the predicate is still a string.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EdgeProposal {
    /// The subject's text.
    pub subject: String,
    /// The subject's kind.
    pub subject_kind: EntityKind,
    /// The predicate as the extractor spelled it.
    pub predicate: String,
    /// The object.
    pub object: ObjectProposal,
    /// Confidence in permille.
    pub confidence: u16,
}

/// Everything an extractor proposes for one memory.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Extraction {
    /// Entity mentions in text order.
    pub mentions: Vec<MentionProposal>,
    /// Proposed edges.
    pub edges: Vec<EdgeProposal>,
}

/// Why a proposed edge was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rejection {
    /// The predicate is not in the closed vocabulary (B-5).
    UnknownPredicate,
    /// The confidence is under [`CONFIDENCE_FLOOR`] (B-8).
    BelowFloor,
    /// An entity the edge names is not among the mentions.
    UnmentionedEntity,
}

/// An edge that passed the vocabulary check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidEdge {
    /// The subject's text.
    pub subject: String,
    /// The subject's kind.
    pub subject_kind: EntityKind,
    /// The predicate.
    pub predicate: Predicate,
    /// The object.
    pub object: ObjectProposal,
    /// Confidence in permille.
    pub confidence: u16,
}

/// A proposal held to the vocabulary and the floor.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Validated {
    /// Mentions at or above the floor.
    pub mentions: Vec<MentionProposal>,
    /// Edges that passed.
    pub edges: Vec<ValidEdge>,
    /// Edges refused, with the proposed predicate text and the reason.
    pub rejected: Vec<(String, Rejection)>,
}

/// A source of proposals (B-8).
pub trait Extractor: Send + Sync {
    /// The extractor's stable name, recorded on every row it produces.
    fn name(&self) -> &str;
    /// The version, bumped whenever its output could change (B-7).
    fn version(&self) -> u32;
    /// Propose mentions and edges for `text`, given the scope's known
    /// entities.
    fn extract(&self, text: &str, known: &[KnownEntity]) -> Extraction;
}

/// Hold `extraction` to the closed vocabulary and the confidence floor.
#[must_use]
pub fn validate(extraction: Extraction) -> Validated {
    let mentions: Vec<MentionProposal> = extraction
        .mentions
        .into_iter()
        .filter(|m| m.confidence >= CONFIDENCE_FLOOR)
        .collect();
    let mentioned = |surface: &str, kind: EntityKind| {
        let wanted = normalize(surface);
        mentions
            .iter()
            .any(|m| m.kind == kind && normalize(&m.surface) == wanted)
    };
    let mut edges = Vec::new();
    let mut rejected = Vec::new();
    for edge in extraction.edges {
        let Some(predicate) = Predicate::parse(&edge.predicate) else {
            rejected.push((edge.predicate, Rejection::UnknownPredicate));
            continue;
        };
        if edge.confidence < CONFIDENCE_FLOOR {
            rejected.push((edge.predicate, Rejection::BelowFloor));
            continue;
        }
        let object_ok = match &edge.object {
            ObjectProposal::Entity { surface, kind } => mentioned(surface, *kind),
            ObjectProposal::Literal(text) => !text.trim().is_empty(),
        };
        if !mentioned(&edge.subject, edge.subject_kind) || !object_ok {
            rejected.push((edge.predicate, Rejection::UnmentionedEntity));
            continue;
        }
        edges.push(ValidEdge {
            subject: edge.subject,
            subject_kind: edge.subject_kind,
            predicate,
            object: edge.object,
            confidence: edge.confidence,
        });
    }
    Validated {
        mentions,
        edges,
        rejected,
    }
}

/// The default extractor: pattern and gazetteer based, deterministic (D-2).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PatternExtractor;

impl PatternExtractor {
    /// The recorded name.
    pub const NAME: &'static str = "pattern";
    /// The recorded version.
    pub const VERSION: u32 = 1;
}

/// Relation phrases, longest first so `is a member of` wins over `is`.
const PHRASES: &[(&[&str], Predicate)] = &[
    (&["is", "a", "member", "of"], Predicate::MemberOf),
    (&["is", "located", "in"], Predicate::LocatedIn),
    (&["is", "related", "to"], Predicate::RelatedTo),
    (&["is", "part", "of"], Predicate::PartOf),
    (&["works", "on"], Predicate::WorksOn),
    (&["works", "at"], Predicate::WorksAt),
    (&["depends", "on"], Predicate::DependsOn),
    (&["supersedes"], Predicate::Supersedes),
    (&["authored"], Predicate::Authored),
    (&["prefers"], Predicate::Prefers),
    (&["decided"], Predicate::Decided),
    (&["caused"], Predicate::Caused),
];

/// Capitalized words that begin sentences without naming anything.
const STOPWORDS: &[&str] = &[
    "A",
    "An",
    "And",
    "But",
    "He",
    "Her",
    "His",
    "I",
    "If",
    "In",
    "It",
    "Its",
    "My",
    "Our",
    "She",
    "So",
    "That",
    "The",
    "Their",
    "Then",
    "There",
    "They",
    "This",
    "Today",
    "Tomorrow",
    "We",
    "When",
    "Yesterday",
    "You",
    "Your",
];

#[derive(Clone, Debug)]
struct Word<'a> {
    text: &'a str,
    start: usize,
    end: usize,
    /// Whether a sentence ended at or before this word's start.
    boundary_before: bool,
}

fn words(text: &str) -> Vec<Word<'_>> {
    let mut out = Vec::new();
    let mut boundary = true;
    let mut cursor = 0;
    for raw in text.split_inclusive(char::is_whitespace) {
        let start = cursor;
        cursor += raw.len();
        let had_newline = raw.ends_with('\n');
        let trimmed_end = raw.trim_end();
        let lead = trimmed_end.len() - trimmed_end.trim_start_matches(['(', '"', '\'', '[']).len();
        let core = trimmed_end
            .trim_start_matches(['(', '"', '\'', '['])
            .trim_end_matches([',', ';', ':', ')', '"', '\'', ']']);
        let sentence_end = core.ends_with(['.', '!', '?']) && !is_initial(core);
        let core = if sentence_end {
            core.trim_end_matches(['.', '!', '?'])
        } else {
            core
        };
        if !core.is_empty() {
            out.push(Word {
                text: core,
                start: start + lead,
                end: start + lead + core.len(),
                boundary_before: boundary,
            });
            boundary = false;
        }
        if sentence_end || had_newline {
            boundary = true;
        }
    }
    out
}

/// `D.` or `Dr.` style: a short capitalized word with a trailing period.
fn is_initial(word: &str) -> bool {
    let bare = word.trim_end_matches('.');
    bare.chars().count() <= 2 && bare.chars().all(char::is_uppercase) && !bare.is_empty()
}

fn capitalized(word: &str) -> bool {
    word.chars().next().is_some_and(char::is_uppercase)
}

/// The maximal capitalized run ending at `end` (exclusive word index), within
/// one sentence, minus leading stopwords. Returns its word range.
fn phrase_before(words: &[Word<'_>], end: usize) -> Option<(usize, usize)> {
    let mut start = end;
    while start > 0 {
        let word = words.get(start - 1)?;
        if !capitalized(word.text) {
            break;
        }
        start -= 1;
        if words.get(start)?.boundary_before {
            break;
        }
    }
    trim_stopwords(words, start, end)
}

/// The maximal capitalized run starting at `start`, within one sentence.
fn phrase_after(words: &[Word<'_>], start: usize) -> Option<(usize, usize)> {
    let mut end = start;
    while let Some(word) = words.get(end) {
        if !capitalized(word.text) || (end > start && word.boundary_before) {
            break;
        }
        end += 1;
    }
    trim_stopwords(words, start, end)
}

fn trim_stopwords(words: &[Word<'_>], mut start: usize, end: usize) -> Option<(usize, usize)> {
    while start < end && STOPWORDS.contains(&words.get(start)?.text) {
        start += 1;
    }
    (start < end).then_some((start, end))
}

fn span(words: &[Word<'_>], (first, last): (usize, usize)) -> Option<(usize, usize)> {
    Some((
        words.get(first)?.start,
        words.get(last.checked_sub(1)?)?.end,
    ))
}

fn matches_phrase(words: &[Word<'_>], at: usize, phrase: &[&str]) -> bool {
    phrase.iter().enumerate().all(|(offset, expected)| {
        words.get(at + offset).is_some_and(|w| {
            w.text.eq_ignore_ascii_case(expected) && (offset == 0 || !w.boundary_before)
        })
    })
}

impl Extractor for PatternExtractor {
    fn name(&self) -> &str {
        Self::NAME
    }

    fn version(&self) -> u32 {
        Self::VERSION
    }

    fn extract(&self, text: &str, known: &[KnownEntity]) -> Extraction {
        let ws = words(text);
        let mut mentions: Vec<MentionProposal> = Vec::new();
        let mut edges = Vec::new();
        let kind_of_known = |surface: &str, fallback: EntityKind| {
            let wanted = normalize(surface);
            known
                .iter()
                .find(|k| k.surfaces.iter().any(|s| normalize(s) == wanted))
                .map_or(fallback, |k| k.kind)
        };
        let add = |mentions: &mut Vec<MentionProposal>,
                   range: (usize, usize),
                   kind: EntityKind,
                   confidence: u16|
         -> Option<(String, EntityKind)> {
            let surface = text.get(range.0..range.1)?.to_owned();
            let kind = kind_of_known(&surface, kind);
            mentions.push(MentionProposal {
                surface: surface.clone(),
                kind,
                start: range.0,
                end: range.1,
                confidence,
            });
            Some((surface, kind))
        };
        let mut index = 0;
        while index < ws.len() {
            let Some(word) = ws.get(index) else { break };
            if let Some(inner) = word
                .text
                .strip_prefix("--")
                .and_then(|rest| rest.strip_suffix("-->"))
                .filter(|inner| !inner.is_empty())
            {
                let subject = phrase_before(&ws, index).and_then(|p| span(&ws, p));
                let object = phrase_after(&ws, index + 1);
                if let (Some(subject), Some(object_words)) = (subject, object)
                    && let Some(object) = span(&ws, object_words)
                {
                    let s = add(&mut mentions, subject, EntityKind::Concept, 900);
                    let o = add(&mut mentions, object, EntityKind::Concept, 900);
                    if let (Some((subject, subject_kind)), Some((surface, kind))) = (s, o) {
                        edges.push(EdgeProposal {
                            subject,
                            subject_kind,
                            predicate: inner.to_owned(),
                            object: ObjectProposal::Entity { surface, kind },
                            confidence: 900,
                        });
                    }
                }
                index += 1;
                continue;
            }
            let Some((phrase, predicate)) = PHRASES
                .iter()
                .find(|(phrase, _)| matches_phrase(&ws, index, phrase))
            else {
                index += 1;
                continue;
            };
            let after = index + phrase.len();
            let subject = phrase_before(&ws, index).and_then(|p| span(&ws, p));
            let Some(subject) = subject else {
                index += 1;
                continue;
            };
            let object = match predicate.object_slot() {
                Slot::Entity(kind) => phrase_after(&ws, after)
                    .and_then(|p| span(&ws, p))
                    .map(|range| (range, kind)),
                Slot::Literal => None,
            };
            let literal = match predicate.object_slot() {
                Slot::Literal => ws.get(after..).and_then(|rest| {
                    let first = rest.first()?;
                    let last = rest
                        .iter()
                        .enumerate()
                        .take_while(|(i, w)| *i == 0 || !w.boundary_before)
                        .last()
                        .map(|(_, w)| w.end)?;
                    text.get(first.start..last)
                        .filter(|t| !t.is_empty() && t.len() <= MAX_LITERAL)
                        .map(str::to_owned)
                }),
                Slot::Entity(_) => None,
            };
            let subject_entry = add(&mut mentions, subject, predicate.subject_kind(), 800);
            let object_entry = match (&object, &literal) {
                (Some((range, kind)), _) => add(&mut mentions, *range, *kind, 800)
                    .map(|(surface, kind)| ObjectProposal::Entity { surface, kind }),
                (None, Some(text)) => Some(ObjectProposal::Literal(text.clone())),
                (None, None) => None,
            };
            if let (Some((subject, subject_kind)), Some(object)) = (subject_entry, object_entry) {
                edges.push(EdgeProposal {
                    subject,
                    subject_kind,
                    predicate: predicate.as_str().to_owned(),
                    object,
                    confidence: 800,
                });
            }
            index = after;
        }
        gazetteer(text, known, &mut mentions);
        mentions.sort_by(|a, b| a.start.cmp(&b.start).then(b.end.cmp(&a.end)));
        let mut kept: Vec<MentionProposal> = Vec::new();
        for mention in mentions {
            if kept.last().is_some_and(|last| mention.start < last.end) {
                continue;
            }
            kept.push(mention);
        }
        Extraction {
            mentions: kept,
            edges,
        }
    }
}

/// Add a mention for every known surface found at word boundaries.
fn gazetteer(text: &str, known: &[KnownEntity], mentions: &mut Vec<MentionProposal>) {
    let mut surfaces: Vec<(&str, EntityKind)> = known
        .iter()
        .flat_map(|k| k.surfaces.iter().map(move |s| (s.as_str(), k.kind)))
        .filter(|(s, _)| !s.is_empty())
        .collect();
    surfaces.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then(a.0.cmp(b.0)));
    for (surface, kind) in surfaces {
        for (start, _) in text.match_indices(surface) {
            let end = start + surface.len();
            let before = text.get(..start).and_then(|t| t.chars().next_back());
            let after = text.get(end..).and_then(|t| t.chars().next());
            if before.is_some_and(char::is_alphanumeric) || after.is_some_and(char::is_alphanumeric)
            {
                continue;
            }
            if mentions.iter().any(|m| start < m.end && m.start < end) {
                // A pattern mention over the same span keeps the known kind
                // (already applied) and its place; the gazetteer adds only
                // spans the patterns did not find.
                continue;
            }
            mentions.push(MentionProposal {
                surface: surface.to_owned(),
                kind,
                start,
                end,
                confidence: 1000,
            });
        }
    }
}
