//! The one error this crate raises (spec 011 FR-002).
//!
//! Every refusal names the field it refused, because the caller that has to
//! act on it is a deserializer several layers away from the value. A message
//! that says "unknown variant" and nothing else sends a reader to the whole
//! record; one that says `kind` sends them to a line.
//!
//! The type maps onto rahi's `Error::Validation` and inherits its exit code
//! (spec 010 B-6). Nothing here invents a code of its own.

use core::fmt;

/// A value that cannot be turned into one of this crate's types.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum TypeError {
    /// A closed taxonomy was handed a string outside it (B-4).
    ///
    /// The model that proposes metadata cannot widen a vocabulary by
    /// emitting a new word: the word is refused here, by name.
    UnknownVariant {
        /// The field whose vocabulary was violated: `kind`, `trust`, `status`.
        field: &'static str,
        /// The string that was offered, quoted back for the reader.
        value: String,
        /// The whole vocabulary, so the message carries the fix.
        expected: &'static [&'static str],
    },
    /// A field a variant requires was absent.
    MissingField {
        /// The field that is missing.
        field: &'static str,
        /// Where it was required, so the message says why it is missing.
        context: &'static str,
    },
    /// A field was present and well-typed but carried a value outside range.
    Invalid {
        /// The field that is out of range.
        field: &'static str,
        /// What is wrong with it.
        reason: String,
    },
}

/// This crate's `Result`, fixed on [`TypeError`].
pub type Result<T> = core::result::Result<T, TypeError>;

impl fmt::Display for TypeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownVariant {
                field,
                value,
                expected,
            } => write!(
                f,
                "{field}: {value:?} is not one of {}",
                Vocabulary(expected)
            ),
            Self::MissingField { field, context } => {
                write!(f, "{field}: required by {context} and absent")
            }
            Self::Invalid { field, reason } => write!(f, "{field}: {reason}"),
        }
    }
}

impl core::error::Error for TypeError {}

impl From<TypeError> for rahi_types::Error {
    /// Every refusal here happens before any effect, which is exactly what
    /// `Validation` means on the chassis (spec 010 B-6).
    fn from(error: TypeError) -> Self {
        Self::Validation(error.to_string())
    }
}

/// A comma-separated vocabulary, rendered without allocating a `Vec`.
struct Vocabulary<'a>(&'a [&'static str]);

impl fmt::Display for Vocabulary<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (position, word) in self.0.iter().enumerate() {
            if position > 0 {
                f.write_str(", ")?;
            }
            write!(f, "{word:?}")?;
        }
        Ok(())
    }
}

/// Refuse `value` against the closed vocabulary `expected` of `field`.
pub(crate) fn unknown_variant(
    field: &'static str,
    value: &str,
    expected: &'static [&'static str],
) -> TypeError {
    TypeError::UnknownVariant {
        field,
        value: value.to_owned(),
        expected,
    }
}

/// Refuse an empty or oversized opaque key.
///
/// Keys reach the SQL of spec 012 as identifiers and the wire of spec 020 as
/// path segments, so the shape is fixed once, here, rather than at each
/// boundary that would otherwise invent its own.
pub(crate) fn validate_key(field: &'static str, value: &str) -> Result<()> {
    const MAX_KEY_LEN: usize = 128;

    if value.is_empty() {
        return Err(TypeError::Invalid {
            field,
            reason: "is empty".to_owned(),
        });
    }
    if value.len() > MAX_KEY_LEN {
        return Err(TypeError::Invalid {
            field,
            reason: format!("is {} bytes, over the {MAX_KEY_LEN} ceiling", value.len()),
        });
    }
    if value.trim() != value {
        return Err(TypeError::Invalid {
            field,
            reason: "is padded with whitespace".to_owned(),
        });
    }
    if value.chars().any(char::is_control) {
        return Err(TypeError::Invalid {
            field,
            reason: "carries a control character".to_owned(),
        });
    }
    Ok(())
}
