//! The query and its configuration (spec 018 B-1, B-3, B-8).

use std::collections::BTreeSet;
use std::time::Duration;

use aicortex_types::Scope;
use rahi_types::UnixSeconds;
use ring::digest;
use serde::Serialize;

use crate::filter::Filter;

/// The four candidate channels (B-2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    /// Nearest chunks by cosine similarity (016 B-2).
    Vector,
    /// BM25 over the text index (016 B-5).
    Text,
    /// A decay over `created`.
    Recency,
    /// Memories reached from entities the query names (017 B-9).
    Graph,
}

impl Channel {
    /// Every channel, in the order ties and traces use.
    pub const ALL: [Self; 4] = [Self::Vector, Self::Text, Self::Recency, Self::Graph];

    /// The stable name.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Vector => "vector",
            Self::Text => "text",
            Self::Recency => "recency",
            Self::Graph => "graph",
        }
    }
}

/// The channels a query runs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ChannelSet(BTreeSet<Channel>);

impl ChannelSet {
    /// All four channels.
    #[must_use]
    pub fn all() -> Self {
        Self(Channel::ALL.into_iter().collect())
    }

    /// Exactly `channels`.
    #[must_use]
    pub fn of(channels: impl IntoIterator<Item = Channel>) -> Self {
        Self(channels.into_iter().collect())
    }

    /// Whether `channel` runs.
    #[must_use]
    pub fn contains(&self, channel: Channel) -> bool {
        self.0.contains(&channel)
    }
}

/// One recall request (B-1).
#[derive(Clone, Debug)]
pub struct Query {
    /// The scope searched; every channel states it in its SQL.
    pub scope: Scope,
    /// The query text.
    pub text: String,
    /// What the query admits.
    pub filter: Filter,
    /// How many memories to return at most.
    pub k: usize,
    /// The channels to run.
    pub channels: ChannelSet,
    /// The budget shared by all channels (B-9).
    pub budget: Duration,
    /// Whether the trace carries every candidate (D-2 of the spec).
    pub explain: bool,
}

impl Query {
    /// A query over all channels with no filter, a two second budget, and a
    /// full trace.
    #[must_use]
    pub fn new(scope: Scope, text: impl Into<String>, k: usize) -> Self {
        Self {
            scope,
            text: text.into(),
            filter: Filter::default(),
            k,
            channels: ChannelSet::all(),
            budget: Duration::from_secs(2),
            explain: true,
        }
    }

    /// A SHA-256 over the scope, text, filter, size, and channels: the key a
    /// persisted trace is stored under, and no body (B-6).
    #[must_use]
    pub fn hash(&self, scope_id: &str) -> String {
        let mut context = digest::Context::new(&digest::SHA256);
        let filter = serde_json::to_string(&self.filter).unwrap_or_default();
        let channels = serde_json::to_string(&self.channels).unwrap_or_default();
        for part in [
            scope_id,
            self.text.as_str(),
            filter.as_str(),
            &self.k.to_string(),
            channels.as_str(),
        ] {
            context.update(&(part.len() as u64).to_le_bytes());
            context.update(part.as_bytes());
        }
        context
            .finish()
            .as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }
}

/// Per-channel weights of the fused sum (B-3).
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Weights {
    /// Vector channel weight.
    pub vector: f64,
    /// Text channel weight.
    pub text: f64,
    /// Recency channel weight.
    pub recency: f64,
    /// Graph channel weight.
    pub graph: f64,
}

impl Weights {
    /// The weight of `channel`.
    #[must_use]
    pub const fn of(&self, channel: Channel) -> f64 {
        match channel {
            Channel::Vector => self.vector,
            Channel::Text => self.text,
            Channel::Recency => self.recency,
            Channel::Graph => self.graph,
        }
    }
}

/// The default weights (B-10). Changing them needs an evaluation run.
pub const DEFAULT_WEIGHTS: Weights = Weights {
    vector: 1.0,
    text: 0.8,
    recency: 0.2,
    graph: 0.6,
};

/// The reciprocal-rank constant `c` of `1 / (c + rank)`.
pub const DEFAULT_RANK_CONSTANT: u32 = 60;

/// Candidates fetched per channel as a multiple of `k`.
pub const DEFAULT_OVERFETCH: usize = 4;

/// The recency decay half-life: one week.
pub const DEFAULT_RECENCY_HALF_LIFE_SECS: u64 = 7 * 24 * 60 * 60;

/// Recency only admits memories younger than this many half-lives, which
/// bounds how far back the channel can reach.
pub const RECENCY_WINDOW_HALF_LIVES: u64 = 4;

/// Entries in each freshness list (B-11).
pub const DEFAULT_FRESHNESS_LIMIT: usize = 16;

/// How the importance prior scales a fused score: `1 - span/2 + span * x`
/// for decayed importance `x` in `0..=1`, so it ranges over
/// `1 +/- span/2` and never zeroes a memory.
pub const DEFAULT_IMPORTANCE_SPAN: f64 = 0.5;

/// What recall is configured with.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct RecallConfig {
    /// Channel weights.
    pub weights: Weights,
    /// The RRF constant.
    pub rank_constant: u32,
    /// Candidates per channel as a multiple of `k`.
    pub overfetch: usize,
    /// Recency half-life in seconds.
    pub recency_half_life_secs: u64,
    /// The importance prior's span.
    pub importance_span: f64,
    /// Entries per freshness list.
    pub freshness_limit: usize,
}

impl Default for RecallConfig {
    fn default() -> Self {
        Self {
            weights: DEFAULT_WEIGHTS,
            rank_constant: DEFAULT_RANK_CONSTANT,
            overfetch: DEFAULT_OVERFETCH,
            recency_half_life_secs: DEFAULT_RECENCY_HALF_LIFE_SECS,
            importance_span: DEFAULT_IMPORTANCE_SPAN,
            freshness_limit: DEFAULT_FRESHNESS_LIMIT,
        }
    }
}

/// The injected clock (B-8).
pub trait Clock: Send + Sync {
    /// The current time.
    fn now(&self) -> UnixSeconds;
}

/// A clock that always reads the same instant.
#[derive(Clone, Copy, Debug)]
pub struct FixedClock(pub UnixSeconds);

impl Clock for FixedClock {
    fn now(&self) -> UnixSeconds {
        self.0
    }
}

/// The wall clock.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> UnixSeconds {
        aicortex_embed::unix_now()
    }
}
