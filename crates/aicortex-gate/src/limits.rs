//! The ceilings (spec 013 B-6).
//!
//! Three of them: how much text a memory may carry, how many media objects it
//! may reference, and which top-level media types are storable at all. A
//! candidate over any of them is refused before a detector runs, because
//! scanning an unbounded body is itself the denial of service the ceiling
//! exists to prevent.
//!
//! The one asymmetry worth naming: a deployment may narrow these and may not
//! widen them ([`Limits::narrowed_to`]). A configurable ceiling that can be
//! raised is not a ceiling, it is a default, and a default is what the
//! operator of a compromised deployment edits first.

use std::collections::BTreeSet;

use aicortex_types::MediaRef;

use crate::GateError;

/// The default text ceiling: 64 KiB (B-6).
///
/// The same number as `aicortex_store::DEFAULT_MAX_BODY_BYTES`, and
/// deliberately not imported from it: the gate does not depend on the store,
/// and the store's ceiling is a second check at the storage boundary
/// (spec 012 B-8) rather than this one. Two independent constants that agree
/// is the point; one constant shared would mean a single edit moves both.
pub const DEFAULT_MAX_BODY_BYTES: usize = 64 * 1024;

/// The default reference ceiling: sixteen media objects on one memory.
pub const DEFAULT_MAX_MEDIA_REFS: usize = 16;

/// The top-level media types a memory may reference by default.
///
/// A top-level type rather than a full type list, because the subtype space
/// is open and a deployment that stores `image/avif` should not need a code
/// change. What this refuses is the type that is not media at all.
pub const DEFAULT_MEDIA_TOP_LEVELS: &[&str] =
    &["application", "audio", "font", "image", "text", "video"];

/// The ceilings, as one value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Limits {
    /// The largest normalized body, in bytes.
    pub max_body_bytes: usize,
    /// The most media references one memory may carry.
    pub max_media_refs: usize,
    /// The top-level media types that may be referenced.
    pub media_top_levels: BTreeSet<String>,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
            max_media_refs: DEFAULT_MAX_MEDIA_REFS,
            media_top_levels: DEFAULT_MEDIA_TOP_LEVELS
                .iter()
                .map(|level| (*level).to_owned())
                .collect(),
        }
    }
}

impl Limits {
    /// The shipped ceilings.
    #[must_use]
    pub fn standard() -> Self {
        Self::default()
    }

    /// The same ceilings with a smaller body ceiling.
    ///
    /// # Errors
    ///
    /// [`GateError::CeilingRaised`] when `bytes` is above the current one.
    pub fn with_max_body_bytes(mut self, bytes: usize) -> Result<Self, GateError> {
        if bytes > self.max_body_bytes {
            return Err(GateError::CeilingRaised {
                ceiling: "max_body_bytes",
                current: self.max_body_bytes,
                asked: bytes,
            });
        }
        self.max_body_bytes = bytes;
        Ok(self)
    }

    /// The same ceilings with a smaller media ceiling.
    ///
    /// # Errors
    ///
    /// [`GateError::CeilingRaised`] when `refs` is above the current one.
    pub fn with_max_media_refs(mut self, refs: usize) -> Result<Self, GateError> {
        if refs > self.max_media_refs {
            return Err(GateError::CeilingRaised {
                ceiling: "max_media_refs",
                current: self.max_media_refs,
                asked: refs,
            });
        }
        self.max_media_refs = refs;
        Ok(self)
    }

    /// `other`, checked to be no wider than `self` in any dimension.
    ///
    /// The type-level expression of "configurable downward only": there is no
    /// path from a [`Limits`] to a wider one, so a deployment's configuration
    /// can only ever subtract.
    ///
    /// # Errors
    ///
    /// [`GateError::CeilingRaised`] naming the first ceiling `other` widens.
    pub fn narrowed_to(self, other: Self) -> Result<Self, GateError> {
        let narrowed = self
            .clone()
            .with_max_body_bytes(other.max_body_bytes)?
            .with_max_media_refs(other.max_media_refs)?;
        if let Some(added) = other
            .media_top_levels
            .difference(&self.media_top_levels)
            .next()
        {
            return Err(GateError::MediaTypeWidened {
                top_level: added.clone(),
            });
        }
        Ok(Self {
            media_top_levels: other.media_top_levels,
            ..narrowed
        })
    }

    /// The top-level type of `media`, which is everything before the slash.
    #[must_use]
    pub fn top_level_of(media: &MediaRef) -> &str {
        media
            .media_type
            .split(['/', ';'])
            .next()
            .unwrap_or_default()
            .trim()
    }

    /// Whether this deployment stores objects of `media`'s type.
    #[must_use]
    pub fn admits_media(&self, media: &MediaRef) -> bool {
        let top_level = Self::top_level_of(media).to_ascii_lowercase();
        !top_level.is_empty()
            && media.media_type.contains('/')
            && self.media_top_levels.contains(&top_level)
    }
}
