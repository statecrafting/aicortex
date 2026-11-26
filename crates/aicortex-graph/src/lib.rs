//! Entities and typed edges: a second axis for recall (spec 017).
//!
//! Similarity answers "what looks like this". The graph answers "what do I
//! know about this person": entities resolved to canonical records with
//! aliases ([`entity`], [`resolve`]), connected by typed edges from a closed
//! predicate vocabulary ([`vocabulary`], [`edge`]), found by an extractor
//! that only proposes ([`extract`]) and committed by a job on the embedding
//! outbox ([`worker`]).
//!
//! Everything here is derived. Every entity and edge names the memory it came
//! from and the extractor version that produced it, a walk ([`walk`]) returns
//! memories with the path that reached them and never an answer of its own,
//! and a derived assertion never outranks the memory it came from (B-6).
//! This crate carries no content out: identifiers, paths, and counts only.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod edge;
pub mod entity;
pub mod extract;
pub mod migrations;
pub mod preflight;
pub mod resolve;
pub mod vocabulary;
pub mod walk;
pub mod worker;

pub use edge::{
    Contradiction, Edge, EdgeId, EdgeRun, EntityRef, PlannedEdge, contradictions, edges_of,
    plan_edges,
};
pub use entity::{Entity, EntityId, MAX_LIST, NewEntity, derived_id, get, list, normalize};
pub use extract::{
    CONFIDENCE_FLOOR, EdgeProposal, Extraction, Extractor, KnownEntity, MentionProposal,
    ObjectProposal, PatternExtractor, Rejection, ValidEdge, Validated, validate,
};
pub use preflight::{GraphPreflight, ScopeGraph, read_deployment, read_scope};
pub use resolve::{
    ContextVector, MERGE_RETENTION_SECONDS, MentionRef, Resolution, SIMILARITY_THRESHOLD, Via,
    merge, resolve, unmerge,
};
pub use vocabulary::{EdgeStatus, EntityKind, Predicate, Slot};
pub use walk::{
    DEFAULT_DEPTH, DEFAULT_FANOUT, Hop, HopEnd, MAX_DEPTH, MAX_FANOUT, Path, WalkFilter, WalkHit,
    WalkOptions, neighbors,
};
pub use worker::{
    DrainReport, ExtractionConfig, ExtractionWorker, ScheduleBatch, schedule, schedule_deployment,
};
