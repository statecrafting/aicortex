//! The entity pointer block of a search result (spec 026 B-5, B-6, B-8, B-9).
//!
//! A search that asks for `pointers` receives, beside its evidence, a small
//! block telling the agent that memory about a named entity exists. The
//! resolution is `aicortex-graph`'s: exact and normalized alias match over the
//! query text in the searched scope, no model call, nothing stored. This
//! module frames what that resolution returns and fits it to its bounds.
//!
//! What the block holds. Every canonical name is derived from stored content,
//! so it appears only inside an envelope of 019's `frame_literal`: a record
//! labelled `extracted_literal`, naming the entity and the memory it came
//! from, with `&`, `<`, and `>` escaped so the name cannot forge a delimiter
//! (019 B-6, B-7). The fields beside that envelope are bare values and never
//! derived from stored text: the entity id, its kind from a closed
//! vocabulary, two counts, a time, and the byte range of the match. The only
//! other text is [`POINTER_NOTE`], a constant of this module. The block
//! carries no framing statement of its own: it is a sibling field of the
//! response's framed memories, which state the versioned framing once for
//! both (019 B-2), ahead of it.
//!
//! Bounds. The block holds at most the configured number of pointers and at
//! most the configured number of tokens, counted by the deployment's declared
//! counter (025 B-5), whose identifier and version the block names itself.
//! Cutting is from the end and is always marked.

use aicortex_graph::{PointerBounds, PointerSet};
use aicortex_recall::delivery::{Companion, DeliveryConfig, TokenCounter};
use aicortex_recall::{LiteralEnvelope, LiteralInput, frame_literal};
use aicortex_store::ScopeId;
use aicortex_types::Scope;
use axum::http::StatusCode;
use rahi_store::StoreHandle;
use serde::Serialize;

use crate::error::{ApiError, ApiResult};

/// The read tools that can retrieve an entity a pointer names (021 B-5).
///
/// `aicortex-mcp` cannot be depended on from here, and it depends on this
/// crate, so its tests read this list and assert that each name is a read
/// tool of its tool list (026 FR-010): a rename there fails there.
pub const POINTER_TOOLS: [&str; 3] = ["fetch", "relate", "search"];

/// The one sentence of a pointer block that is not a value.
///
/// It names [`POINTER_TOOLS`], interpolates nothing stored, and tells the
/// agent what exists and what could read it, never that it must act (026 B-5).
pub const POINTER_NOTE: &str = "Stored memory exists in this scope about each entity listed here. The fetch, relate, and search tools can read it.";

/// The tokens a block may hold when a deployment names no bound (026 B-6).
pub const DEFAULT_MAX_TOKENS: u64 = 400;

/// The fewest tokens a configuration may bound a block to: enough for the
/// counter's name and the truncation marks with no pointer.
pub const MIN_TOKENS: u64 = 128;

/// The most tokens a configuration may bound a block to.
pub const MAX_TOKENS: u64 = 8_000;

/// What a deployment configures for pointers (026 B-6, B-8).
///
/// The default is enabled, with 5 pointers, 400 tokens, and 8 KiB of query
/// text. A request still has to ask: pointers are off for every request that
/// does not carry `pointers: true` (026 D-3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PointerConfig {
    enabled: bool,
    bounds: PointerBounds,
    max_tokens: u64,
}

impl Default for PointerConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            bounds: PointerBounds::default(),
            max_tokens: DEFAULT_MAX_TOKENS,
        }
    }
}

impl PointerConfig {
    /// An enabled configuration: at most `max_pointers` pointers and
    /// `max_tokens` tokens over the first `text_bytes` bytes of a query. Each
    /// is clamped into its range (`aicortex-graph`'s bounds, and
    /// [`MIN_TOKENS`] to [`MAX_TOKENS`]).
    #[must_use]
    pub fn new(max_pointers: usize, max_tokens: u64, text_bytes: usize) -> Self {
        Self {
            enabled: true,
            bounds: PointerBounds::new(max_pointers, text_bytes),
            max_tokens: max_tokens.clamp(MIN_TOKENS, MAX_TOKENS),
        }
    }

    /// The same configuration, switched on or off. A deployment that switches
    /// it off answers a request for pointers with no block and a field
    /// saying so (026 B-8).
    #[must_use]
    pub const fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Whether this deployment serves pointers.
    #[must_use]
    pub const fn enabled(&self) -> bool {
        self.enabled
    }

    /// The bounds of one resolution.
    #[must_use]
    pub const fn bounds(&self) -> PointerBounds {
        self.bounds
    }

    /// The most tokens a block holds.
    #[must_use]
    pub const fn max_tokens(&self) -> u64 {
        self.max_tokens
    }
}

/// The counter the budget was measured in, named by the block itself because
/// a search that asks for pointers without delivery carries no other counter
/// field (026 B-6, 025 B-3).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CounterOut {
    /// The counter's identifier.
    pub id: String,
    /// Its version.
    pub version: u32,
    /// The configured counter that could not run, when this one stands in
    /// for it (025 B-5).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_from: Option<String>,
}

/// The byte range of a match in the query text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct RangeOut {
    /// Where the match starts.
    pub start: u64,
    /// Where it ends, exclusive.
    pub end: u64,
}

/// One pointer: bare values beside the enveloped name.
#[derive(Serialize)]
pub struct PointerOut {
    /// The entity, readable through `GET /entities/{id}` and `relate`.
    pub entity_id: String,
    /// What it is (017 B-1): a label of a closed vocabulary.
    pub kind: &'static str,
    /// How many `active` memories mention it.
    pub memories: u64,
    /// When the newest of them was captured, in Unix seconds.
    pub last_seen: u64,
    /// Where in the query text it was named.
    pub range: RangeOut,
    /// The canonical name, inside an envelope (019 B-6): derived from stored
    /// content, so never a bare string.
    pub name: LiteralEnvelope,
}

/// The pointer block (026 B-5, B-6).
///
/// It serializes as the whole of what it holds; [`Companion`] lets 025's
/// delivery stage ask what a shorter block would cost, so the block is fitted
/// into the budget the evidence leaves (026 B-9).
pub struct PointerBlock {
    counter: CounterOut,
    set_truncated: bool,
    text_truncated: bool,
    entries: Vec<PointerOut>,
}

/// A block as it serializes with its first `keep` entries.
#[derive(Serialize)]
struct View<'a> {
    counter: &'a CounterOut,
    truncated: bool,
    text_truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<&'static str>,
    pointers: &'a [PointerOut],
}

impl PointerBlock {
    fn view(&self, keep: usize) -> View<'_> {
        let keep = keep.min(self.entries.len());
        View {
            counter: &self.counter,
            truncated: self.set_truncated || keep < self.entries.len(),
            text_truncated: self.text_truncated,
            note: (keep > 0).then_some(POINTER_NOTE),
            pointers: self.entries.get(..keep).unwrap_or_default(),
        }
    }

    /// The tokens `counter` counts in the block with its first `keep` entries.
    fn count(&self, counter: &dyn TokenCounter, keep: usize) -> Result<u64, serde_json::Error> {
        Ok(counter.count(&serde_json::to_string(&self.view(keep))?))
    }

    /// Frame `set` and fit it into `max_tokens` of `counter`'s units.
    ///
    /// Pointers are cut from the end until the serialized block is within the
    /// bound, and the block is marked truncated. `None` when not even the
    /// block with no pointer is within the bound, so nothing is returned
    /// rather than something over it.
    ///
    /// # Errors
    ///
    /// An internal problem when an envelope cannot be encoded.
    pub fn build(
        set: PointerSet,
        counter: &dyn TokenCounter,
        fallback_from: Option<String>,
        max_tokens: u64,
    ) -> ApiResult<Option<Self>> {
        let enveloping = |_| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "an entity pointer could not be enveloped",
            )
        };
        let mut entries = Vec::with_capacity(set.pointers.len());
        for pointer in &set.pointers {
            let name = frame_literal(LiteralInput::new(
                pointer.entity.as_str(),
                &pointer.canonical,
                pointer.trust,
                std::slice::from_ref(&pointer.newest),
            ))
            .map_err(enveloping)?;
            entries.push(PointerOut {
                entity_id: pointer.entity.to_string(),
                kind: pointer.kind.label(),
                memories: pointer.memories,
                last_seen: pointer.last_seen.get(),
                range: RangeOut {
                    start: pointer.start,
                    end: pointer.end,
                },
                name,
            });
        }
        let mut block = Self {
            counter: CounterOut {
                id: counter.id().to_owned(),
                version: counter.version(),
                fallback_from,
            },
            set_truncated: set.truncated,
            text_truncated: set.text_truncated,
            entries,
        };
        for keep in (0..=block.entries.len()).rev() {
            let tokens = block.count(counter, keep).map_err(enveloping)?;
            if tokens <= max_tokens {
                block.cut(keep);
                return Ok(Some(block));
            }
        }
        Ok(None)
    }

    /// Keep the first `keep` pointers and mark the block truncated if that
    /// dropped any (026 B-9).
    pub fn cut(&mut self, keep: usize) {
        if keep < self.entries.len() {
            self.entries.truncate(keep);
            self.set_truncated = true;
        }
    }

    /// How many pointers the block holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the block holds no pointer.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl Serialize for PointerBlock {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.view(self.entries.len()).serialize(serializer)
    }
}

impl Companion for PointerBlock {
    fn entries(&self) -> usize {
        self.entries.len()
    }

    fn tokens(&self, counter: &dyn TokenCounter, keep: usize) -> Result<u64, serde_json::Error> {
        self.count(counter, keep)
    }
}

/// Resolve and frame the pointers `query` names in `scope` (026 B-1, B-7).
///
/// The caller has already admitted the search for `scope`, under the same
/// `memory.read` requirement as the evidence (026 B-7). Nothing is persisted,
/// embedded, traced, or logged here (026 B-2).
///
/// # Errors
///
/// Store errors, or an envelope that cannot be encoded.
pub async fn resolve(
    store: &StoreHandle,
    scope: &Scope,
    query: &str,
    config: &PointerConfig,
    delivery: &DeliveryConfig,
) -> ApiResult<Option<PointerBlock>> {
    let set = aicortex_graph::pointers(store, ScopeId::of(scope).as_str(), query, &config.bounds())
        .await?;
    let (counter, fallback_from) = delivery.effective_counter();
    PointerBlock::build(set, counter.as_ref(), fallback_from, config.max_tokens())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use aicortex_recall::Utf8BytesDiv3;

    fn empty(truncated: bool) -> PointerSet {
        PointerSet {
            pointers: Vec::new(),
            truncated,
            text_truncated: false,
        }
    }

    #[test]
    fn an_empty_set_is_a_block_with_no_note() {
        let block = PointerBlock::build(empty(false), &Utf8BytesDiv3, None, DEFAULT_MAX_TOKENS)
            .unwrap()
            .expect("an empty block fits");
        let text = serde_json::to_value(&block).unwrap();
        assert_eq!(text["counter"]["id"], "utf8-bytes-div3");
        assert_eq!(text["pointers"], serde_json::json!([]));
        assert!(text.get("note").is_none());
        assert_eq!(text["truncated"], false);
        assert!(block.is_empty());
    }

    #[test]
    fn a_block_over_the_bound_even_when_empty_is_omitted() {
        let block = PointerBlock::build(empty(false), &Utf8BytesDiv3, None, 1).unwrap();
        assert!(block.is_none());
    }

    #[test]
    fn the_note_names_every_pointer_tool_and_asks_for_nothing() {
        for tool in POINTER_TOOLS {
            assert!(POINTER_NOTE.contains(tool), "{tool}");
        }
        let lowered = POINTER_NOTE.to_lowercase();
        for imperative in ["must", "should", "you ", "ignore", "always", "never"] {
            assert!(!lowered.contains(imperative), "{imperative}");
        }
    }

    #[test]
    fn configuration_clamps_its_token_bound() {
        assert_eq!(PointerConfig::new(5, 1, 1).max_tokens(), MIN_TOKENS);
        assert_eq!(PointerConfig::new(5, u64::MAX, 1).max_tokens(), MAX_TOKENS);
        assert!(PointerConfig::default().enabled());
        assert!(!PointerConfig::default().with_enabled(false).enabled());
    }
}
