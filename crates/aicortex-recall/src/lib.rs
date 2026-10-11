//! Recalled content is data, never an instruction (spec 019).
//!
//! Client surfaces serialize [`FramedMemories`], produced by [`frame_memories`],
//! instead of serializing stored memories. Ranking and recall traces arrive
//! under spec 018; this crate currently establishes only the content boundary.

pub mod envelope;

pub use envelope::{
    EnvelopeInput, FRAMING_STATEMENT, FRAMING_VERSION, FramedMemories, frame_memories,
};
