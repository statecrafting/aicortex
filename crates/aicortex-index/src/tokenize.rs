//! Tokenization for the text channel (spec 016 B-6).
//!
//! A pure function from text to tokens. Every stored posting was produced by
//! it, so changing what it emits for any input changes the index and is a
//! migration: [`TOKENIZER_VERSION`] names the behaviour, and the fixtures in
//! `tests/text.rs` fail on any change to it (FR-005).
//!
//! The steps, in order:
//!
//! 1. Unicode NFKC, so full-width and compatibility forms meet their
//!    ordinary spelling.
//! 2. Word segmentation. A word is a run of letters, digits, and combining
//!    marks; an apostrophe between two letters stays inside the word and is
//!    then dropped (`don't` is `dont`), and a trailing possessive `'s` is
//!    removed. Every other character separates words, so hyphenated
//!    compounds and `snake_case` identifiers split into their parts. Han
//!    ideographs, kana, and other scripts written without spaces are emitted
//!    one character per token, the unigram segmentation UAX #29 also
//!    applies to ideographs.
//! 3. A word with internal case changes (`parseHttpRequest`, `XMLParser`)
//!    is emitted whole and then as each of its camel-case parts, so a search
//!    for the identifier and a search for one of its words both match.
//! 4. Unicode lowercase.
//! 5. For [`Language::English`]: the stop-word list, then (when enabled) a
//!    light plural stemmer that only removes `s`, `es`, and `ies` endings.
//! 6. Tokens longer than the maximum length are dropped.

use unicode_normalization::UnicodeNormalization;

/// The tokenizer behaviour stored postings were produced with.
pub const TOKENIZER_VERSION: u32 = 1;

/// The longest token kept, in characters. Longer runs are dropped: they are
/// hashes, encoded blobs, or URLs with the punctuation stripped, and match
/// nothing a person types.
pub const DEFAULT_MAX_TOKEN_CHARS: usize = 64;

/// The language whose stop words and stemmer apply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Language {
    /// English stop words and the light English plural stemmer.
    English,
    /// No stop words and no stemming: segmentation and case folding only.
    Neutral,
}

/// Tokenizer configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TokenizerConfig {
    /// The language for stop words and stemming.
    pub language: Language,
    /// Whether the light stemmer runs (English only).
    pub stem: bool,
    /// Tokens longer than this many characters are dropped.
    pub max_token_chars: usize,
}

impl Default for TokenizerConfig {
    fn default() -> Self {
        Self {
            language: Language::English,
            stem: true,
            max_token_chars: DEFAULT_MAX_TOKEN_CHARS,
        }
    }
}

/// English stop words, sorted for binary search.
const ENGLISH_STOP_WORDS: &[&str] = &[
    "a",
    "about",
    "above",
    "after",
    "again",
    "against",
    "all",
    "am",
    "an",
    "and",
    "any",
    "are",
    "as",
    "at",
    "be",
    "because",
    "been",
    "before",
    "being",
    "below",
    "between",
    "both",
    "but",
    "by",
    "can",
    "did",
    "do",
    "does",
    "doing",
    "down",
    "during",
    "each",
    "few",
    "for",
    "from",
    "further",
    "had",
    "has",
    "have",
    "having",
    "he",
    "her",
    "here",
    "hers",
    "herself",
    "him",
    "himself",
    "his",
    "how",
    "i",
    "if",
    "in",
    "into",
    "is",
    "it",
    "its",
    "itself",
    "just",
    "me",
    "more",
    "most",
    "my",
    "myself",
    "no",
    "nor",
    "not",
    "now",
    "of",
    "off",
    "on",
    "once",
    "only",
    "or",
    "other",
    "our",
    "ours",
    "ourselves",
    "out",
    "over",
    "own",
    "same",
    "she",
    "should",
    "so",
    "some",
    "such",
    "than",
    "that",
    "the",
    "their",
    "theirs",
    "them",
    "themselves",
    "then",
    "there",
    "these",
    "they",
    "this",
    "those",
    "through",
    "to",
    "too",
    "under",
    "until",
    "up",
    "very",
    "was",
    "we",
    "were",
    "what",
    "when",
    "where",
    "which",
    "while",
    "who",
    "whom",
    "why",
    "will",
    "with",
    "you",
    "your",
    "yours",
    "yourself",
    "yourselves",
];

/// Tokenize `text` under `config`, in reading order, duplicates kept.
#[must_use]
pub fn tokenize(text: &str, config: &TokenizerConfig) -> Vec<String> {
    let normalized: String = text.nfkc().collect();
    let mut tokens = Vec::new();
    for word in words(&normalized) {
        match word {
            Word::Single(c) => push(&mut tokens, c.to_lowercase().collect(), config),
            Word::Run(run) => {
                let parts = camel_parts(&run);
                let whole: String = run.iter().collect::<String>().to_lowercase();
                if parts.len() > 1 {
                    push(&mut tokens, whole, config);
                    for part in parts {
                        push(&mut tokens, part.to_lowercase(), config);
                    }
                } else {
                    push(&mut tokens, whole, config);
                }
            }
        }
    }
    tokens
}

/// Apply stop words, stemming, and the length limit, then keep the token.
fn push(tokens: &mut Vec<String>, token: String, config: &TokenizerConfig) {
    if token.is_empty() || token.chars().count() > config.max_token_chars {
        return;
    }
    let token = match config.language {
        Language::Neutral => token,
        Language::English => {
            if ENGLISH_STOP_WORDS.binary_search(&token.as_str()).is_ok() {
                return;
            }
            if config.stem {
                stem_english(token)
            } else {
                token
            }
        }
    };
    tokens.push(token);
}

/// A segmented word: a run of word characters, or one character of a
/// script written without spaces.
enum Word {
    Run(Vec<char>),
    Single(char),
}

/// Han ideographs, kana, and the other scripts emitted one character at a
/// time.
fn is_unspaced(c: char) -> bool {
    matches!(u32::from(c),
        0x3040..=0x309F      // Hiragana
        | 0x30A0..=0x30FF    // Katakana
        | 0x31F0..=0x31FF    // Katakana phonetic extensions
        | 0x3400..=0x4DBF    // CJK extension A
        | 0x4E00..=0x9FFF    // CJK unified ideographs
        | 0xF900..=0xFAFF    // CJK compatibility ideographs
        | 0x20000..=0x3134F  // CJK extensions B to G
    )
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || is_mark(c)
}

/// Combining marks: they belong to the letter before them.
fn is_mark(c: char) -> bool {
    use unicode_general_category::{GeneralCategory, get_general_category};
    matches!(
        get_general_category(c),
        GeneralCategory::NonspacingMark
            | GeneralCategory::SpacingMark
            | GeneralCategory::EnclosingMark
    )
}

fn is_apostrophe(c: char) -> bool {
    c == '\'' || c == '\u{2019}'
}

fn words(text: &str) -> Vec<Word> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut run: Vec<char> = Vec::new();
    let mut previous: Option<char> = None;
    let mut iter = chars.iter().copied().peekable();
    while let Some(c) = iter.next() {
        if is_unspaced(c) {
            flush(&mut run, &mut out);
            out.push(Word::Single(c));
        } else if is_word_char(c) {
            run.push(c);
        } else if is_apostrophe(c)
            && previous.is_some_and(char::is_alphabetic)
            && iter
                .peek()
                .is_some_and(|next| next.is_alphabetic() && !is_unspaced(*next))
        {
            // Inside a word: `don't`, `o'clock`. A possessive `'s` that ends
            // the word is dropped with its apostrophe.
            let possessive = iter.peek().is_some_and(|next| *next == 's' || *next == 'S');
            if possessive {
                let mut lookahead = iter.clone();
                lookahead.next();
                if lookahead.peek().is_none_or(|after| !is_word_char(*after)) {
                    iter.next();
                    previous = Some('s');
                    flush(&mut run, &mut out);
                    continue;
                }
            }
        } else {
            flush(&mut run, &mut out);
        }
        previous = Some(c);
    }
    flush(&mut run, &mut out);
    out
}

fn flush(run: &mut Vec<char>, out: &mut Vec<Word>) {
    if !run.is_empty() {
        out.push(Word::Run(std::mem::take(run)));
    }
}

/// Split a word at its camel-case boundaries: a lowercase letter or digit
/// followed by an uppercase letter (`parseHttp`), and the last capital of an
/// uppercase run followed by a lowercase letter (`XMLParser`). A word with
/// no boundary is one part.
fn camel_parts(run: &[char]) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    for (index, c) in run.iter().copied().enumerate() {
        let previous = index.checked_sub(1).and_then(|i| run.get(i)).copied();
        let next = run.get(index + 1).copied();
        let boundary = c.is_uppercase()
            && previous.is_some_and(|p| {
                p.is_lowercase()
                    || p.is_numeric()
                    || (p.is_uppercase() && next.is_some_and(char::is_lowercase))
            });
        if boundary && !current.is_empty() {
            parts.push(std::mem::take(&mut current));
        }
        current.push(c);
    }
    if !current.is_empty() {
        parts.push(current);
    }
    parts
}

/// A light English plural stemmer: `ies` to `y`, `es` to `e`, and a final
/// `s` dropped, each only where the ending is unambiguous. ASCII letters
/// only, and only words of four letters or more.
fn stem_english(token: String) -> String {
    if token.len() < 4 || !token.bytes().all(|b| b.is_ascii_lowercase()) {
        return token;
    }
    if let Some(stem) = token.strip_suffix("ies")
        && !stem.ends_with('e')
        && !stem.ends_with('a')
    {
        return format!("{stem}y");
    }
    if let Some(stem) = token.strip_suffix("es")
        && !stem.ends_with('a')
        && !stem.ends_with('e')
        && !stem.ends_with('o')
    {
        return format!("{stem}e");
    }
    if let Some(stem) = token.strip_suffix('s')
        && !stem.ends_with('u')
        && !stem.ends_with('s')
    {
        return stem.to_owned();
    }
    token
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing)]

    use super::*;

    #[test]
    fn the_stop_word_list_is_sorted_for_binary_search() {
        assert!(ENGLISH_STOP_WORDS.windows(2).all(|pair| pair[0] < pair[1]));
    }
}
