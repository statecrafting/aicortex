//! Where in a stored source a claim's value was read (spec 050 B-12, B-13).
//!
//! A span points into a part of a multi-part source by offsets and names a
//! keyed digest of that whole part, so a reader holding the scope can tell
//! whether the bytes are still the bytes the span was measured against. It
//! carries no content: offsets and a digest only (I-5). The digest is
//! HMAC-SHA-256 under a key held per scope in the application store, as
//! 013 B-9 holds its keys, so a digest of a short part cannot be confirmed by
//! guessing, and erasing the scope destroys the key (D-6). This module holds
//! the reference; minting the key and computing the digest belong to the
//! writer that stores spans.

use serde::{Deserialize, Serialize};

use crate::claim_time::{checked_string, closed_vocabulary};
use crate::error::{Result, TypeError, validate_key};
use crate::id::MemoryId;

closed_vocabulary! {
    /// The kinds of part a span can point into.
    PartKind, "part", PART_KINDS {
        /// The source's body.
        Body => "body",
        /// A MIME part, by its dotted path.
        Mime => "mime",
        /// An attachment, by its name.
        Attachment => "attachment",
    }
}

/// The part of a multi-part source a span points into (B-12).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "RawPart", into = "RawPart")]
pub enum PartLocator {
    /// The body.
    Body,
    /// A MIME part path such as `1.2`.
    Mime(String),
    /// An attachment name.
    Attachment(String),
}

impl PartLocator {
    /// A MIME part path: positive integers joined by `.`.
    ///
    /// # Errors
    ///
    /// [`TypeError::Invalid`] naming `part` for any other shape.
    pub fn mime(path: impl Into<String>) -> Result<Self> {
        let path = path.into();
        let legal = !path.is_empty()
            && path.split('.').all(|segment| {
                !segment.is_empty()
                    && !segment.starts_with('0')
                    && segment.bytes().all(|byte| byte.is_ascii_digit())
            });
        if !legal {
            return Err(TypeError::Invalid {
                field: "part",
                reason: format!("{path:?} is not a MIME part path"),
            });
        }
        Ok(Self::Mime(path))
    }

    /// An attachment name.
    ///
    /// # Errors
    ///
    /// [`TypeError::Invalid`] naming `part` for an empty, padded, oversized
    /// or control-bearing name.
    pub fn attachment(name: impl Into<String>) -> Result<Self> {
        let name = name.into();
        validate_key("part", &name)?;
        Ok(Self::Attachment(name))
    }

    /// Which kind of part.
    #[must_use]
    pub const fn kind(&self) -> PartKind {
        match self {
            Self::Body => PartKind::Body,
            Self::Mime(_) => PartKind::Mime,
            Self::Attachment(_) => PartKind::Attachment,
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPart {
    kind: PartKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
}

impl From<PartLocator> for RawPart {
    fn from(part: PartLocator) -> Self {
        let kind = part.kind();
        match part {
            PartLocator::Body => Self {
                kind,
                path: None,
                name: None,
            },
            PartLocator::Mime(path) => Self {
                kind,
                path: Some(path),
                name: None,
            },
            PartLocator::Attachment(name) => Self {
                kind,
                path: None,
                name: Some(name),
            },
        }
    }
}

impl TryFrom<RawPart> for PartLocator {
    type Error = TypeError;

    fn try_from(raw: RawPart) -> Result<Self> {
        match (raw.kind, raw.path, raw.name) {
            (PartKind::Body, None, None) => Ok(Self::Body),
            (PartKind::Mime, Some(path), None) => Self::mime(path),
            (PartKind::Attachment, None, Some(name)) => Self::attachment(name),
            (kind, ..) => Err(TypeError::Invalid {
                field: "part",
                reason: format!(
                    "a {kind} part carries {}",
                    match kind {
                        PartKind::Body => "no other field",
                        PartKind::Mime => "`path` and nothing else",
                        PartKind::Attachment => "`name` and nothing else",
                    }
                ),
            }),
        }
    }
}

closed_vocabulary! {
    /// What a span's offsets count.
    SpanUnit, "unit", SPAN_UNITS {
        /// Bytes of the part as stored.
        Bytes => "bytes",
        /// Unicode scalar values of the part.
        Chars => "chars",
    }
}

/// A half-open range `[start, end)` over a part, saying what it counts
/// (B-12).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "RawRange", into = "RawRange")]
pub struct SpanRange {
    unit: SpanUnit,
    start: u64,
    end: u64,
}

impl SpanRange {
    /// `[start, end)` counted in `unit`.
    ///
    /// # Errors
    ///
    /// [`TypeError::Invalid`] naming `range` when the range is empty or
    /// reversed.
    pub fn new(unit: SpanUnit, start: u64, end: u64) -> Result<Self> {
        if end <= start {
            return Err(TypeError::Invalid {
                field: "range",
                reason: format!("[{start}, {end}) is empty"),
            });
        }
        Ok(Self { unit, start, end })
    }

    /// What the offsets count.
    #[must_use]
    pub const fn unit(&self) -> SpanUnit {
        self.unit
    }

    /// The first offset inside the range.
    #[must_use]
    pub const fn start(&self) -> u64 {
        self.start
    }

    /// The first offset past the range.
    #[must_use]
    pub const fn end(&self) -> u64 {
        self.end
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRange {
    unit: SpanUnit,
    start: u64,
    end: u64,
}

impl From<SpanRange> for RawRange {
    fn from(range: SpanRange) -> Self {
        Self {
            unit: range.unit,
            start: range.start,
            end: range.end,
        }
    }
}

impl TryFrom<RawRange> for SpanRange {
    type Error = TypeError;

    fn try_from(raw: RawRange) -> Result<Self> {
        Self::new(raw.unit, raw.start, raw.end)
    }
}

checked_string! {
    /// Lowercase hex, as a digest and a key id are written.
    Hex, "digest", lower_hex
}

fn lower_hex(field: &'static str, text: &str) -> Result<()> {
    let legal = !text.is_empty()
        && text.len().is_multiple_of(2)
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
    if legal {
        Ok(())
    } else {
        Err(TypeError::Invalid {
            field,
            reason: format!("{text:?} is not lowercase hex"),
        })
    }
}

/// A keyed digest of a whole part: the algorithm, the id of the per-scope
/// key, and the digest (B-12). Never the content, never the key.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContentDigest {
    /// The construction, `HMAC-SHA-256` today.
    pub algorithm: String,
    /// The per-scope key that produced it.
    pub key_id: Hex,
    /// The digest.
    pub digest: Hex,
}

/// A reference into a stored source: which memory, which part, which range,
/// and the digest of the part it was measured against (B-12).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceSpan {
    /// The stored source.
    pub source: MemoryId,
    /// The part of it.
    pub part: PartLocator,
    /// The range within the part.
    pub range: SpanRange,
    /// The keyed digest of the whole part.
    pub digest: ContentDigest,
}
