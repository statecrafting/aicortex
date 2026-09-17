//! The memory identifier (spec 011 B-1).
//!
//! A [`MemoryId`] is a UUIDv7, so the identifier sorts by the moment it was
//! minted and a b-tree over it stays local as rows arrive. The writer mints
//! it: the row that spec 012 inserts already knows its own id, which is what
//! lets the memory, its provenance, its derivation rows, and its outbox work
//! land in one transaction instead of waiting on a generated key.

use core::fmt;
use core::str::FromStr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::{Result, TypeError};

/// The identity of one memory: a UUIDv7, minted by the writer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MemoryId(Uuid);

impl MemoryId {
    /// Mint an identifier for a memory being written now.
    ///
    /// This is the one place in the crate that reads a clock, and it reads it
    /// because a v7 identifier is a timestamp with randomness after it. It
    /// performs no I/O.
    #[must_use]
    pub fn now_v7() -> Self {
        Self(Uuid::now_v7())
    }

    /// Carry an identifier that was minted elsewhere: a row being read back,
    /// an import that preserves the source's own v7 value.
    #[must_use]
    pub const fn from_uuid(uuid: Uuid) -> Self {
        Self(uuid)
    }

    /// The identifier as a UUID.
    #[must_use]
    pub const fn as_uuid(&self) -> &Uuid {
        &self.0
    }

    /// Parse a hyphenated UUID.
    ///
    /// # Errors
    ///
    /// [`TypeError::Invalid`] naming `id` when the text is not a UUID.
    pub fn parse(text: &str) -> Result<Self> {
        Uuid::parse_str(text)
            .map(Self)
            .map_err(|error| TypeError::Invalid {
                field: "id",
                reason: format!("{text:?} is not a uuid: {error}"),
            })
    }
}

impl FromStr for MemoryId {
    type Err = TypeError;

    fn from_str(text: &str) -> Result<Self> {
        Self::parse(text)
    }
}

impl fmt::Display for MemoryId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.as_hyphenated())
    }
}

impl From<Uuid> for MemoryId {
    fn from(uuid: Uuid) -> Self {
        Self(uuid)
    }
}
