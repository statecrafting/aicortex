//! Recall: four channels fused into one order, every result explainable
//! (spec 018), and the content boundary every result crosses (spec 019).
//!
//! [`Recall::recall`] returns memory ids, ranks, and a [`RecallTrace`]. It
//! never returns a body. Client surfaces serialize [`FramedMemories`],
//! produced by [`frame_memories`], instead of serializing stored memories.

#![allow(clippy::float_arithmetic)]

pub mod channels;
pub mod envelope;
pub mod filter;
pub mod freshness;
pub mod fusion;
pub mod query;
pub mod recall;
pub mod trace;

pub use envelope::{
    EnvelopeInput, FRAMING_STATEMENT, FRAMING_VERSION, FramedMemories, LiteralEnvelope,
    LiteralInput, frame_literal, frame_memories,
};
pub use filter::{Extra, Filter};
pub use freshness::{EntityFreshness, Freshness, SourceFreshness};
pub use fusion::{fuse, recompute_order};
pub use query::{
    Channel, ChannelSet, Clock, DEFAULT_WEIGHTS, FixedClock, Query, RecallConfig, SystemClock,
    Weights,
};
pub use recall::{Recall, RecallResult, RecalledMemory};
pub use trace::{ByteRange, ChannelTrace, Contribution, FusedEntry, RecallTrace, TraceCandidate};
