//! The recall trace (spec 018 B-5, B-6, D-2).
//!
//! A trace holds ids, scores, ranks, and counts: never a memory body, entity
//! name, or alias. That is what makes it safe to persist (B-6) and why it
//! sits outside spec 019's envelope.

use std::collections::BTreeMap;

use aicortex_types::MemoryId;
use serde::Serialize;

use crate::filter::Filter;
use crate::freshness::Freshness;
use crate::query::{Channel, Weights};

/// The byte range of the winning chunk within its memory (B-4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct ByteRange {
    /// First byte.
    pub start: u64,
    /// One past the last byte.
    pub end: u64,
}

/// One channel candidate, ranked from 1.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct TraceCandidate {
    /// The memory.
    pub memory_id: MemoryId,
    /// Its rank within the channel, from 1.
    pub rank: u32,
    /// The channel's raw score.
    pub score: f64,
    /// The best chunk's range, for the vector and text channels.
    pub chunk: Option<ByteRange>,
    /// Path length from the query's entity, for the graph channel.
    pub hops: Option<u8>,
}

/// What one channel contributed.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ChannelTrace {
    /// The channel.
    pub channel: Channel,
    /// How many candidates it returned.
    pub count: usize,
    /// Whether the budget cut it short (B-9).
    pub truncated: bool,
    /// Why it did not run, when it did not.
    pub skipped: Option<String>,
    /// The candidates; empty unless the query asked to explain.
    pub candidates: Vec<TraceCandidate>,
}

/// One channel's term in a fused score.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Contribution {
    /// The channel.
    pub channel: Channel,
    /// The memory's rank in it.
    pub rank: u32,
    /// The channel's weight.
    pub weight: f64,
    /// `weight / (rank_constant + rank)`.
    pub term: f64,
}

/// One returned memory and why it made the cut.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FusedEntry {
    /// The memory.
    pub memory_id: MemoryId,
    /// Its place in the result, from 1.
    pub rank: u32,
    /// The sum of the terms times the prior.
    pub score: f64,
    /// The decayed importance prior (011 B-9).
    pub prior: f64,
    /// The winning chunk's range, when a chunk channel supplied it.
    pub chunk: Option<ByteRange>,
    /// The strongest contributing channel and its rank.
    pub reason: String,
    /// The terms; empty unless the query asked to explain.
    pub contributions: Vec<Contribution>,
}

/// The explanation of a result set (B-5).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RecallTrace {
    /// The query's hash; the key a persisted trace is stored under.
    pub query_hash: String,
    /// The filter, including any extra statuses asked for (B-7).
    pub filters: Filter,
    /// One entry per channel in [`Channel::ALL`] order.
    pub per_channel: Vec<ChannelTrace>,
    /// The returned memories in order.
    pub fused: Vec<FusedEntry>,
    /// The weights in force.
    pub weights: Weights,
    /// The RRF constant in force.
    pub rank_constant: u32,
    /// The importance prior of every fused candidate; empty unless explained.
    pub priors: BTreeMap<MemoryId, f64>,
    /// Milliseconds spent.
    pub budget_used_ms: u64,
    /// Whether any channel was cut short.
    pub truncated: bool,
    /// The freshness block (B-11), copied from the response.
    pub freshness: Freshness,
}

impl RecallTrace {
    /// The trace as it is persisted: JSON of ids, scores, and ranks (B-6).
    ///
    /// # Errors
    ///
    /// Serialization failure, which a trace of plain data cannot produce.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }
}
