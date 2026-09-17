//! What is offered to the gate, and how well its origin is known (spec 013
//! B-7).
//!
//! A candidate is a memory that has not happened yet: every field the record
//! of 011 requires, plus the one thing 011 cannot carry because it is not a
//! property of the memory but of the call that offered it, which is how much
//! the caller could show for where it came from.
//!
//! The gate has no clock and no randomness (B-10), so the identifier and the
//! two timestamps are the caller's. That is not a weakening: the caller is
//! the request path, which has both, and a gate that minted them would be a
//! gate whose verdict on the same bytes differed between two runs.

use aicortex_types::MemoryParts;
use serde::Serialize;

/// How well the source of a candidate is established (B-7).
///
/// Ordered from most to least: the first two are admitted, the third is
/// quarantined, and the fourth is refused. Nothing here is a claim about the
/// *content*; it is a claim about the channel that delivered it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    /// The caller authenticated against rauthy and is the source: an MCP
    /// client, the HTTP API, an agent holding a bearer token.
    Authenticated,
    /// A source adapter this deployment registered, whose identity the
    /// deployment attested when it registered it (spec 030).
    RegisteredAdapter,
    /// The caller named a source that cannot be checked: a pasted export, an
    /// unauthenticated webhook that only proved it knew a shared secret. The
    /// content may be perfectly good, and nothing about the channel says so.
    Asserted,
    /// No origin at all. Constitution IX says such a memory is not stored.
    Unestablished,
}

impl Origin {
    /// The stable name, as a refusal and a fixture record it.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Authenticated => "authenticated",
            Self::RegisteredAdapter => "registered_adapter",
            Self::Asserted => "asserted",
            Self::Unestablished => "unestablished",
        }
    }

    /// Whether this channel establishes the source well enough to admit.
    #[must_use]
    pub const fn is_established(self) -> bool {
        matches!(self, Self::Authenticated | Self::RegisteredAdapter)
    }

    /// Whether this channel is good enough to quarantine rather than refuse.
    #[must_use]
    pub const fn is_assertable(self) -> bool {
        matches!(self, Self::Asserted)
    }
}

/// A memory that has been offered but not yet judged.
#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    /// Everything the record of 011 needs, with the body as it arrived.
    pub parts: MemoryParts,
    /// How well the caller established the source (B-7).
    pub origin: Origin,
}

impl Candidate {
    /// Offer `parts`, delivered over a channel of `origin`.
    #[must_use]
    pub const fn new(parts: MemoryParts, origin: Origin) -> Self {
        Self { parts, origin }
    }
}
