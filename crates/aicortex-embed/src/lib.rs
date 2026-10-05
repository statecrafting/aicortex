//! Versioned embeddings as durable, governed work (spec 015).
//!
//! A capture integration calls [`stage_embedding`] in the same transaction as
//! the memory write. [`EmbeddingWorker`] then claims that work through Rahi's fenced
//! processing queue, computes chunks and vectors outside the transaction,
//! and commits all derived rows with the claim completion. A lost delivery
//! is therefore retried, while a stale worker commits nothing.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod chunk;
pub mod local;
pub mod migrations;
pub mod provider;
pub mod registry;
pub mod remote;
pub mod static_model;
pub mod wordpiece;
pub mod worker;

pub use chunk::{Chunk, ChunkConfig, Chunker};
pub use local::{LocalEngine, LocalProvider, WeightArtifact, WeightFetcher};
pub use migrations::{EMBEDDING_MIGRATION_VERSION, migration};
pub use provider::{EmbeddingProvider, ModelId, Vector};
pub use registry::{Coverage, ModelRegistry, ModelRevision};
pub use remote::{EmbeddingTransport, RemoteProvider};
pub use static_model::StaticEmbeddingEngine;
pub use wordpiece::WordPiece;
pub use worker::{
    ActiveEmbedding, EMBEDDING_NAMESPACE, EMBEDDING_PROCESSOR, EmbeddingPreflight, EmbeddingScope,
    EmbeddingWorker, MAX_REEMBED_BATCH, QueueHealth, ReembeddingBatch, WorkerConfig, WorkerReport,
    embedding_processor, embedding_work_key, queue_health, stage_active_embedding, stage_embedding,
    stage_live_embedding, stage_reembedding_batch,
};
