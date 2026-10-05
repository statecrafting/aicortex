//! A WordPiece tokenizer for the sentence models `LocalProvider` runs.
//!
//! It reads the Hugging Face `tokenizer.json` document and supports exactly
//! the BERT family subset: an optional `BertNormalizer`, an optional
//! `BertPreTokenizer`, and a `WordPiece` model. Any other component is a
//! configuration error at boot, never a silent approximation, because a
//! tokenizer that disagrees with the one the model was trained with produces
//! plausible but wrong vectors.

use std::collections::BTreeMap;

use rahi_types::Error;
use serde::Deserialize;
use unicode_general_category::{GeneralCategory, get_general_category};
use unicode_normalization::UnicodeNormalization;

const DEFAULT_PREFIX: &str = "##";
const DEFAULT_MAX_WORD_CHARS: usize = 100;

#[derive(Debug, Deserialize)]
struct Document {
    normalizer: Option<Component>,
    pre_tokenizer: Option<Component>,
    model: ModelSection,
}

#[derive(Debug, Deserialize)]
struct Component {
    #[serde(rename = "type")]
    kind: String,
    lowercase: Option<bool>,
    strip_accents: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct ModelSection {
    #[serde(rename = "type")]
    kind: String,
    unk_token: Option<String>,
    continuing_subword_prefix: Option<String>,
    max_input_chars_per_word: Option<usize>,
    vocab: BTreeMap<String, u32>,
}

/// A validated WordPiece vocabulary and its text normalization.
#[derive(Clone, Debug)]
pub struct WordPiece {
    vocab: BTreeMap<String, u32>,
    unk_id: u32,
    prefix: String,
    max_word_chars: usize,
    lowercase: bool,
    strip_accents: bool,
    split_punctuation: bool,
}

impl WordPiece {
    /// Parse a `tokenizer.json` document.
    ///
    /// # Errors
    ///
    /// [`Error::Config`] for malformed JSON, a component outside the
    /// supported subset, or an unknown-token name absent from the vocabulary.
    pub fn from_json(bytes: &[u8]) -> Result<Self, Error> {
        let document: Document = serde_json::from_slice(bytes)
            .map_err(|error| Error::Config(format!("tokenizer.json is not valid: {error}")))?;
        if document.model.kind != "WordPiece" {
            return Err(Error::Config(format!(
                "tokenizer model {:?} is unsupported; only WordPiece is",
                document.model.kind
            )));
        }
        let (lowercase, strip_accents) = match &document.normalizer {
            None => (false, false),
            Some(component) if component.kind == "BertNormalizer" => {
                let lowercase = component.lowercase.unwrap_or(true);
                (lowercase, component.strip_accents.unwrap_or(lowercase))
            }
            Some(component) => {
                return Err(Error::Config(format!(
                    "tokenizer normalizer {:?} is unsupported",
                    component.kind
                )));
            }
        };
        let split_punctuation = match &document.pre_tokenizer {
            None => false,
            Some(component) if component.kind == "BertPreTokenizer" => true,
            Some(component) => {
                return Err(Error::Config(format!(
                    "tokenizer pre-tokenizer {:?} is unsupported",
                    component.kind
                )));
            }
        };
        let unk = document
            .model
            .unk_token
            .unwrap_or_else(|| "[UNK]".to_owned());
        let unk_id = *document.model.vocab.get(&unk).ok_or_else(|| {
            Error::Config(format!("tokenizer vocabulary has no unknown token {unk:?}"))
        })?;
        Ok(Self {
            vocab: document.model.vocab,
            unk_id,
            prefix: document
                .model
                .continuing_subword_prefix
                .unwrap_or_else(|| DEFAULT_PREFIX.to_owned()),
            max_word_chars: document
                .model
                .max_input_chars_per_word
                .unwrap_or(DEFAULT_MAX_WORD_CHARS),
            lowercase,
            strip_accents,
            split_punctuation,
        })
    }

    /// The id that stands for text outside the vocabulary.
    #[must_use]
    pub const fn unknown_id(&self) -> u32 {
        self.unk_id
    }

    /// One more than the largest id in the vocabulary.
    #[must_use]
    pub fn id_bound(&self) -> u64 {
        self.vocab
            .values()
            .max()
            .map_or(0, |max| u64::from(*max).saturating_add(1))
    }

    /// Token ids for `text`, without special tokens, at most `limit` long.
    #[must_use]
    pub fn encode(&self, text: &str, limit: usize) -> Vec<u32> {
        let mut ids = Vec::new();
        for word in self.words(text) {
            if ids.len() >= limit {
                break;
            }
            self.push_word(&word, &mut ids);
        }
        ids.truncate(limit);
        ids
    }

    fn normalize(&self, text: &str) -> String {
        let mut normalized = String::with_capacity(text.len());
        let chars: Box<dyn Iterator<Item = char>> = if self.strip_accents {
            Box::new(text.nfd().filter(|c| !is_combining_mark(*c)))
        } else {
            Box::new(text.chars())
        };
        for c in chars {
            if is_removable_control(c) {
                continue;
            }
            if self.lowercase {
                normalized.extend(c.to_lowercase());
            } else {
                normalized.push(c);
            }
        }
        normalized
    }

    fn words(&self, text: &str) -> Vec<String> {
        let normalized = self.normalize(text);
        let mut words = Vec::new();
        let mut current = String::new();
        for c in normalized.chars() {
            if c.is_whitespace() {
                flush(&mut current, &mut words);
            } else if self.split_punctuation && (is_punctuation(c) || is_cjk(c)) {
                flush(&mut current, &mut words);
                words.push(c.to_string());
            } else {
                current.push(c);
            }
        }
        flush(&mut current, &mut words);
        words
    }

    fn push_word(&self, word: &str, ids: &mut Vec<u32>) {
        if word.chars().count() > self.max_word_chars {
            ids.push(self.unk_id);
            return;
        }
        let mut pieces = Vec::new();
        let mut start = 0_usize;
        while start < word.len() {
            let mut found = None;
            let mut ends: Vec<usize> = word
                .char_indices()
                .map(|(index, c)| index.saturating_add(c.len_utf8()))
                .filter(|end| *end > start)
                .collect();
            ends.reverse();
            for end in ends {
                let Some(slice) = word.get(start..end) else {
                    continue;
                };
                let id = if start == 0 {
                    self.vocab.get(slice)
                } else {
                    self.vocab.get(&format!("{}{slice}", self.prefix))
                };
                if let Some(id) = id {
                    found = Some((*id, end));
                    break;
                }
            }
            match found {
                Some((id, end)) => {
                    pieces.push(id);
                    start = end;
                }
                None => {
                    ids.push(self.unk_id);
                    return;
                }
            }
        }
        ids.extend(pieces);
    }
}

fn flush(current: &mut String, words: &mut Vec<String>) {
    if !current.is_empty() {
        words.push(std::mem::take(current));
    }
}

/// Non-spacing marks (category Mn), which BERT strips after NFD.
fn is_combining_mark(c: char) -> bool {
    get_general_category(c) == GeneralCategory::NonspacingMark
}

/// BERT splits on every ASCII symbol and on Unicode category P. Symbols
/// outside ASCII (category S: currency, math, emoji) stay with their word.
fn is_punctuation(c: char) -> bool {
    c.is_ascii_punctuation()
        || matches!(
            get_general_category(c),
            GeneralCategory::ConnectorPunctuation
                | GeneralCategory::DashPunctuation
                | GeneralCategory::OpenPunctuation
                | GeneralCategory::ClosePunctuation
                | GeneralCategory::InitialPunctuation
                | GeneralCategory::FinalPunctuation
                | GeneralCategory::OtherPunctuation
        )
}

/// Control (Cc) and format (Cf) characters, which BERT's cleaning drops;
/// private-use and unassigned code points are kept. Whitespace
/// controls (tab, newline, carriage return) are kept as separators.
fn is_removable_control(c: char) -> bool {
    c == '\0'
        || c == '\u{fffd}'
        || (!c.is_whitespace()
            && matches!(
                get_general_category(c),
                GeneralCategory::Control | GeneralCategory::Format
            ))
}

fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x4e00..=0x9fff | 0x3400..=0x4dbf | 0x20000..=0x2a6df | 0x2a700..=0x2b73f
        | 0x2b740..=0x2b81f | 0x2b820..=0x2ceaf | 0xf900..=0xfaff | 0x2f800..=0x2fa1f)
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    fn tokenizer() -> WordPiece {
        let vocab = [
            "[UNK]", "hello", "world", "un", "##aff", "##able", "!", ",", "cafe", "a",
        ];
        let entries: Vec<String> = vocab
            .iter()
            .enumerate()
            .map(|(id, token)| format!("{token:?}: {id}"))
            .collect();
        let json = format!(
            r#"{{"normalizer":{{"type":"BertNormalizer","lowercase":true}},
                "pre_tokenizer":{{"type":"BertPreTokenizer"}},
                "model":{{"type":"WordPiece","unk_token":"[UNK]","vocab":{{{}}}}}}}"#,
            entries.join(",")
        );
        WordPiece::from_json(json.as_bytes()).expect("tokenizer")
    }

    #[test]
    fn splits_words_punctuation_and_subwords() {
        let tokenizer = tokenizer();
        assert_eq!(tokenizer.encode("Hello, world!", 16), vec![1, 7, 2, 6]);
        assert_eq!(tokenizer.encode("unaffable", 16), vec![3, 4, 5]);
    }

    #[test]
    fn strips_accents_and_marks_unknown_words() {
        let tokenizer = tokenizer();
        assert_eq!(tokenizer.encode("Caf\u{e9}", 16), vec![8]);
        assert_eq!(tokenizer.encode("zzz", 16), vec![0]);
        assert_eq!(tokenizer.encode("unaffz", 16), vec![0]);
    }

    #[test]
    fn strips_marks_of_every_script_and_splits_only_on_punctuation() {
        let tokenizer = tokenizer();
        // Hebrew points and Arabic harakat are Mn, so they vanish; the
        // base letters are unknown and so are not "hello".
        assert_eq!(tokenizer.encode("hello\u{5b0}", 16), vec![1]);
        assert_eq!(tokenizer.encode("hello\u{64e}", 16), vec![1]);
        // A currency sign is a symbol, not punctuation: it stays attached,
        // so the word is no longer "hello" and falls to unknown.
        assert_eq!(tokenizer.encode("hello\u{20ac}", 16), vec![0]);
        // Ideographic punctuation is category P and splits.
        assert_eq!(tokenizer.encode("hello\u{3002}world", 16), vec![1, 0, 2]);
        // Format characters are dropped; private-use code points are not.
        assert_eq!(tokenizer.encode("hel\u{200b}lo", 16), vec![1]);
        assert_eq!(tokenizer.encode("hello\u{e001}", 16), vec![0]);
    }

    #[test]
    fn truncates_to_the_limit() {
        let tokenizer = tokenizer();
        assert_eq!(tokenizer.encode("hello hello hello", 2), vec![1, 1]);
        assert!(tokenizer.encode("", 4).is_empty());
    }

    #[test]
    fn unsupported_components_are_refused() {
        let json =
            br#"{"normalizer":{"type":"NFKC"},"model":{"type":"WordPiece","vocab":{"[UNK]":0}}}"#;
        assert!(WordPiece::from_json(json).is_err());
        let json = br#"{"model":{"type":"BPE","vocab":{"[UNK]":0}}}"#;
        assert!(WordPiece::from_json(json).is_err());
        let json = br#"{"model":{"type":"WordPiece","vocab":{"x":0}}}"#;
        assert!(WordPiece::from_json(json).is_err());
    }
}
