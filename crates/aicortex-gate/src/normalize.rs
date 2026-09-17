//! Normalization, which runs before anything is evaluated (spec 013 B-8).
//!
//! Three steps, in this order:
//!
//! 1. **The invisible characters are removed.** Zero-width and bidirectional
//!    formatting codepoints are deleted outright. This is a safety property
//!    and not tidiness: those characters are how an instruction is hidden
//!    inside text that will later be shown to a model, and how a right-to-left
//!    override makes a rendered line say something other than what its bytes
//!    say. A detector that scanned the raw text would be reading a different
//!    string from the one a reader sees; removing them first makes the two the
//!    same string.
//! 2. **Unicode NFC.** Composed form, so that two spellings of the same
//!    grapheme are one string and a detector cannot be evaded by decomposing
//!    a prefix.
//! 3. **Trailing whitespace is stripped**, per line and then from the end of
//!    the text.
//!
//! The order matters. Removing the formatting characters before composing
//! means a zero-width joiner cannot survive as part of a composed sequence,
//! and trimming last means step 1 cannot leave a line that is now only
//! spaces.
//!
//! Every step is a total function on `&str` with no clock, no allocation
//! beyond the result, and no configuration, so [`text`] is deterministic
//! (B-10).

use aicortex_types::MemoryBody;
use unicode_normalization::UnicodeNormalization;

/// The codepoints step 1 deletes (D-3).
///
/// Deliberately a table rather than a Unicode category test. `Cf` (format)
/// would also catch the interlinear annotation characters and every
/// unassigned future addition to the category, and `default_ignorable` would
/// catch the variation selectors that carry real meaning in an emoji
/// sequence. What is listed is what hides text: the zero-width family, the
/// bidirectional overrides and isolates, and the two characters that render
/// as nothing and are routinely used as separators.
///
/// The zero-width joiner is in the list because B-8 names zero-width
/// characters and because it is the one that concatenates a hidden token onto
/// a visible word. The cost is that a joined emoji sequence
/// (`U+1F468 U+200D U+1F469`) normalizes to its components. That is a
/// deliberate trade of rendering fidelity for the guarantee that what the
/// detectors scan is what a reader sees.
const REMOVED: [char; 16] = [
    '\u{00ad}', // soft hyphen
    '\u{180e}', // Mongolian vowel separator
    '\u{200b}', // zero-width space
    '\u{200c}', // zero-width non-joiner
    '\u{200d}', // zero-width joiner
    '\u{200e}', // left-to-right mark
    '\u{200f}', // right-to-left mark
    '\u{202a}', // left-to-right embedding
    '\u{202b}', // right-to-left embedding
    '\u{202c}', // pop directional formatting
    '\u{202d}', // left-to-right override
    '\u{202e}', // right-to-left override
    '\u{2060}', // word joiner
    '\u{2066}', // left-to-right isolate
    '\u{2067}', // right-to-left isolate
    '\u{2068}', // first strong isolate
];

/// One more, kept out of [`REMOVED`] only because the array is sized: the
/// pop-directional-isolate that closes `U+2066`..`U+2068`.
const POP_DIRECTIONAL_ISOLATE: char = '\u{2069}';

/// The byte-order mark, which is a zero-width no-break space anywhere but at
/// the very start of a stream.
const ZERO_WIDTH_NO_BREAK_SPACE: char = '\u{feff}';

/// Whether `character` is deleted by step 1.
#[must_use]
pub fn is_removed(character: char) -> bool {
    REMOVED.contains(&character)
        || character == POP_DIRECTIONAL_ISOLATE
        || character == ZERO_WIDTH_NO_BREAK_SPACE
}

/// The normalized form of `raw` (B-8).
#[must_use]
pub fn text(raw: &str) -> String {
    let stripped: String = raw.chars().filter(|c| !is_removed(*c)).collect();
    let composed: String = stripped.nfc().collect();
    let mut out = String::with_capacity(composed.len());
    for (index, line) in composed.split('\n').enumerate() {
        if index > 0 {
            out.push('\n');
        }
        out.push_str(line.trim_end());
    }
    let trimmed = out.trim_end();
    if trimmed.len() == out.len() {
        out
    } else {
        trimmed.to_owned()
    }
}

/// The normalized body: the text and the title, with the media untouched.
///
/// A media reference is a digest and a type, neither of which is prose, so
/// there is nothing in one for step 1 to hide behind.
#[must_use]
pub fn body(raw: &MemoryBody) -> MemoryBody {
    MemoryBody {
        text: text(&raw.text),
        title: raw.title.as_deref().map(text).filter(|t| !t.is_empty()),
        media: raw.media.clone(),
    }
}
