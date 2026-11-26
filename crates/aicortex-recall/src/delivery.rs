//! Evidence delivery under a token budget that counts the framing (spec 025).
//!
//! Recall (018) returns ids in a fused order. This stage runs after it: it
//! reads what each result's unit needs, frames it through 019's envelope, and
//! packs the envelopes into a hard cap measured in a declared counter's
//! units over the whole serialized payload, framing statement included. It
//! never changes the order and never re-ranks.
//!
//! What the stage does not do: persist expanded text (B-13), call out of the
//! process (the counters here are formulas), or read outside the caller's
//! scope (B-9).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Arc;

use aicortex_embed::ModelRegistry;
use aicortex_store::provenance_repo::{ProvenanceRepo, TurnRange};
use aicortex_store::{MemoryRepo, ScopeId};
use aicortex_types::{Memory, MemoryId, Scope};
use rahi_store::{StoreHandle, Value};
use rahi_types::Error;
use serde::{Deserialize, Serialize};

use crate::envelope::{EvidenceInput, FramedMemories, frame_evidence, frame_memories};
use crate::recall::{RecallResult, RecalledMemory};
use crate::trace::ByteRange;

/// The widest window a request may name (B-1).
pub const MAX_WINDOW_RADIUS: u8 = 3;

/// The default budget when a request asks for delivery without naming one
/// (B-8), in the counter's units. Provisional until 040 reports a baseline
/// (D-9).
pub const DEFAULT_TOKEN_BUDGET: u64 = 6_000;

/// The largest budget a request may name (B-8), in the counter's units.
pub const MAX_TOKEN_BUDGET: u64 = 32_000;

/// The reason a result that never fit is listed as dropped (B-6 step 3).
pub const DROPPED_BUDGET: &str = "budget";

/// The reason a result that no longer exists is listed as dropped (D-9).
pub const DROPPED_UNAVAILABLE: &str = "unavailable";

/// The unit of evidence a result is delivered as (B-1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(tag = "unit", rename_all = "snake_case")]
pub enum EvidenceUnit {
    /// The winning chunk of 018 B-4, with its byte range.
    Chunk,
    /// The winning chunk and `radius` neighbors each side; for a turn of a
    /// conversation, the turns within `radius` of it.
    Window {
        /// Neighbors on each side, 1 to [`MAX_WINDOW_RADIUS`].
        radius: u8,
    },
    /// The whole body; for a turn of a conversation, the whole conversation.
    Record,
    /// `Record` for a conversational memory and `Chunk` for any other.
    Auto,
    /// The hit turn of a conversation as its whole message. Reported only;
    /// a request that names it is refused (B-1).
    Turn,
}

/// Which names a request may use for a unit.
const UNIT_NAMES: &str = "chunk, window, record, auto";

/// A unit as a request names it, before its radius is attached.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Named {
    Chunk,
    Window,
    Record,
    Auto,
}

/// Why a delivery request was refused.
#[derive(Debug)]
pub enum DeliveryError {
    /// The unit is not one a request may name. `turn` is delivered, never
    /// requested (B-1).
    UnknownUnit {
        /// The name offered.
        given: String,
    },
    /// The radius is outside 1 to [`MAX_WINDOW_RADIUS`].
    WindowRadius {
        /// The radius offered.
        given: u8,
    },
    /// The budget cannot hold the framing statement, one envelope, a cut
    /// marker, and one character of body (B-7).
    BudgetBelowMinimum {
        /// The budget requested, after the server ceiling.
        budget: u64,
        /// The smallest budget that can deliver this result.
        minimum: u64,
    },
    /// The configured default budget exceeds the configured maximum.
    Config {
        /// The default budget.
        default_budget: u64,
        /// The maximum budget.
        max_budget: u64,
    },
    /// An envelope could not be serialized.
    Frame(String),
    /// The store failed.
    Store(Error),
}

impl fmt::Display for DeliveryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownUnit { given } => write!(
                f,
                "{given:?} is not a unit a request can name; use one of {UNIT_NAMES}"
            ),
            Self::WindowRadius { given } => write!(
                f,
                "window_radius {given} is outside 1 to {MAX_WINDOW_RADIUS}"
            ),
            Self::BudgetBelowMinimum { budget, minimum } => write!(
                f,
                "token_budget {budget} cannot hold the framing, one envelope, a cut marker, and one \
                 character of body; the minimum for this query is {minimum}"
            ),
            Self::Config {
                default_budget,
                max_budget,
            } => write!(
                f,
                "the default token budget {default_budget} exceeds the maximum {max_budget}; \
                 set the default at or below the maximum"
            ),
            Self::Frame(reason) => write!(f, "an envelope could not be serialized: {reason}"),
            Self::Store(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for DeliveryError {}

impl From<Error> for DeliveryError {
    fn from(error: Error) -> Self {
        Self::Store(error)
    }
}

impl From<serde_json::Error> for DeliveryError {
    fn from(error: serde_json::Error) -> Self {
        Self::Frame(error.to_string())
    }
}

/// A counter that cannot run (B-5): a vocabulary that is missing or fails to
/// load.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CounterUnavailable(pub String);

/// The one declared, in-process token counter of a delivery (B-5).
///
/// It makes no outbound call. Its identifier and version travel in every
/// response that carries delivery, so a reader can tell which units the
/// budget was enforced in. Counting must be monotone: a longer text never
/// counts fewer tokens.
pub trait TokenCounter: Send + Sync {
    /// The counter's identifier.
    fn id(&self) -> &str;

    /// The counter's version.
    fn version(&self) -> u32;

    /// Whether the counter can run. An operator's model tokenizer reports
    /// here that its vocabulary is missing or fails to load; the stage then
    /// counts with [`Utf8Bytes`] and says so.
    ///
    /// # Errors
    ///
    /// [`CounterUnavailable`] naming why.
    fn ready(&self) -> Result<(), CounterUnavailable> {
        Ok(())
    }

    /// The tokens in `text`. Called only when [`Self::ready`] is `Ok`.
    fn count(&self, text: &str) -> u64;
}

/// The default counter: `ceil(bytes / 3)` over UTF-8 (B-5, D-6).
#[derive(Clone, Copy, Debug, Default)]
pub struct Utf8BytesDiv3;

impl TokenCounter for Utf8BytesDiv3 {
    fn id(&self) -> &str {
        "utf8-bytes-div3"
    }

    fn version(&self) -> u32 {
        1
    }

    fn count(&self, text: &str) -> u64 {
        (text.len() as u64).div_ceil(3)
    }
}

/// One token per UTF-8 byte (B-5): never under-counts a byte-level tokenizer,
/// and the fallback when a configured counter cannot run.
#[derive(Clone, Copy, Debug, Default)]
pub struct Utf8Bytes;

impl TokenCounter for Utf8Bytes {
    fn id(&self) -> &str {
        "utf8-bytes"
    }

    fn version(&self) -> u32 {
        1
    }

    fn count(&self, text: &str) -> u64 {
        text.len() as u64
    }
}

/// What a deployment configures (B-5, B-8).
#[derive(Clone)]
pub struct DeliveryConfig {
    default_budget: u64,
    max_budget: u64,
    counter: Arc<dyn TokenCounter>,
}

impl fmt::Debug for DeliveryConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeliveryConfig")
            .field("default_budget", &self.default_budget)
            .field("max_budget", &self.max_budget)
            .field("counter", &self.counter.id())
            .finish()
    }
}

impl Default for DeliveryConfig {
    fn default() -> Self {
        Self {
            default_budget: DEFAULT_TOKEN_BUDGET,
            max_budget: MAX_TOKEN_BUDGET,
            counter: Arc::new(Utf8BytesDiv3),
        }
    }
}

impl DeliveryConfig {
    /// A configuration with `counter`, a default budget, and a ceiling.
    ///
    /// # Errors
    ///
    /// [`DeliveryError::Config`] when the default exceeds the maximum.
    pub fn new(
        default_budget: u64,
        max_budget: u64,
        counter: Arc<dyn TokenCounter>,
    ) -> Result<Self, DeliveryError> {
        if default_budget > max_budget {
            return Err(DeliveryError::Config {
                default_budget,
                max_budget,
            });
        }
        Ok(Self {
            default_budget,
            max_budget,
            counter,
        })
    }
}

/// A request's delivery parameters (B-3, B-14).
///
/// A request carrying none of a unit, a radius, or a budget does not ask for
/// delivery, and [`Delivery::deliver`] returns `None` for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeliveryRequest {
    unit: Option<Named>,
    radius: Option<u8>,
    budget: Option<u64>,
}

impl DeliveryRequest {
    /// Validate the parameters as a surface received them.
    ///
    /// # Errors
    ///
    /// [`DeliveryError::UnknownUnit`] for any name but `chunk`, `window`,
    /// `record`, and `auto` (`turn` included); [`DeliveryError::WindowRadius`]
    /// for a radius outside 1 to [`MAX_WINDOW_RADIUS`].
    pub fn parse(
        unit: Option<&str>,
        window_radius: Option<u8>,
        token_budget: Option<u64>,
    ) -> Result<Self, DeliveryError> {
        let unit = unit
            .map(|name| match name {
                "chunk" => Ok(Named::Chunk),
                "window" => Ok(Named::Window),
                "record" => Ok(Named::Record),
                "auto" => Ok(Named::Auto),
                other => Err(DeliveryError::UnknownUnit {
                    given: other.to_owned(),
                }),
            })
            .transpose()?;
        if let Some(radius) = window_radius
            && !(1..=MAX_WINDOW_RADIUS).contains(&radius)
        {
            return Err(DeliveryError::WindowRadius { given: radius });
        }
        Ok(Self {
            unit,
            radius: window_radius,
            budget: token_budget,
        })
    }

    /// Whether the request asks for delivery at all (B-3).
    #[must_use]
    pub const fn is_requested(&self) -> bool {
        self.unit.is_some() || self.radius.is_some() || self.budget.is_some()
    }

    fn radius(&self) -> u8 {
        self.radius.unwrap_or(1)
    }

    /// The unit before `Auto` is resolved per memory; `Chunk` by default.
    fn requested(&self) -> EvidenceUnit {
        match self.unit.unwrap_or(Named::Chunk) {
            Named::Chunk => EvidenceUnit::Chunk,
            Named::Window => EvidenceUnit::Window {
                radius: self.radius(),
            },
            Named::Record => EvidenceUnit::Record,
            Named::Auto => EvidenceUnit::Auto,
        }
    }
}

/// Why a result was delivered as it was, in order of precedence (B-11).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    /// A turn already delivered inside an earlier result's conversation unit.
    Covered,
    /// Delivered as a cut chunk with a cut marker.
    Cut,
    /// Delivered as a `Window` or `Record` over turns.
    Conversation,
    /// Delivered at a lower rung than requested.
    Reduced,
    /// Delivered at the unit requested.
    Fit,
}

/// A conversation turn a result carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct TurnReport {
    /// The turn's memory.
    pub memory_id: MemoryId,
    /// Its turn number.
    pub turn: u32,
}

/// The counter a response names (B-5).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CounterReport {
    /// The identifier of the counter that enforced the budget.
    pub id: String,
    /// Its version.
    pub version: u32,
    /// The configured counter that could not run and was replaced by
    /// `utf8-bytes`, with why.
    pub fallback_from: Option<String>,
}

/// One result, as delivered (B-11).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ResultReport {
    /// The result's memory.
    pub memory_id: MemoryId,
    /// Its place in the fused order, from 1.
    pub rank: u32,
    /// The unit requested for it, with `Auto` resolved.
    pub requested: EvidenceUnit,
    /// The unit delivered; none for a covered result.
    pub delivered: Option<EvidenceUnit>,
    /// Why.
    pub reason: Reason,
    /// The delivered byte ranges, one per envelope, in payload order.
    pub byte_ranges: Vec<ByteRange>,
    /// The turns it added to the payload; empty for a memory that is not a
    /// turn, and the carrying unit's turns for a covered result.
    pub turns: Vec<TurnReport>,
    /// Turns inside its unit that an earlier unit already delivered.
    pub shared_turns: Vec<TurnReport>,
    /// For a covered result, the result whose unit carried it.
    pub covered_by: Option<MemoryId>,
    /// Its share of the payload, in the counter's units; zero when covered.
    pub tokens: u64,
}

/// A result that is not in the payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct DroppedReport {
    /// The memory.
    pub memory_id: MemoryId,
    /// [`DROPPED_BUDGET`] or [`DROPPED_UNAVAILABLE`].
    pub reason: &'static str,
}

/// What a delivery reports beside the framed payload (B-11, B-13).
///
/// Ids, ranges, and counts only, never text, so it is safe to persist beside
/// the recall trace.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DeliveryReport {
    /// The counter the budget was enforced in.
    pub counter: CounterReport,
    /// The budget in force, after the server ceiling.
    pub budget: u64,
    /// Whether the request named more than the server's maximum.
    pub budget_clamped: bool,
    /// The recounted payload, in the counter's units.
    pub used: u64,
    /// One entry per fused result that is in the payload or covered by it.
    pub results: Vec<ResultReport>,
    /// The results that are not, by id.
    pub dropped: Vec<DroppedReport>,
}

/// The framed payload and its report.
pub struct Delivered {
    /// What the model reads: the framing statement and the envelopes.
    pub payload: FramedMemories,
    /// What a client and the trace read.
    pub report: DeliveryReport,
}

/// A memory and its erasure marker.
struct Item {
    memory: Memory,
    erased: bool,
}

/// One envelope of the payload.
#[derive(Clone)]
struct Piece {
    memory: Memory,
    erased: bool,
    range: ByteRange,
    cut: bool,
}

impl Piece {
    fn input(&self) -> Result<EvidenceInput<'_>, DeliveryError> {
        EvidenceInput::new(&self.memory, self.erased, self.range, self.cut).ok_or_else(|| {
            DeliveryError::Frame(format!(
                "range {}..{} is not inside the body of memory {}",
                self.range.start, self.range.end, self.memory.id
            ))
        })
    }
}

/// What one result needs, read once.
struct Loaded {
    hit: Item,
    hit_turn: Option<u32>,
    chunk: ByteRange,
    chunks: Vec<ByteRange>,
    others: BTreeMap<u32, Item>,
}

/// One result's walk down the ladder, as accepted.
struct Accepted {
    memory_id: MemoryId,
    rank: u32,
    requested: EvidenceUnit,
    delivered: EvidenceUnit,
    conversational: bool,
    pieces: Vec<Piece>,
    shared: Vec<TurnReport>,
    cut: bool,
}

/// Counting and the per-envelope cost cache.
struct Meter<'a> {
    counter: &'a dyn TokenCounter,
    costs: BTreeMap<(MemoryId, u64, u64, bool), u64>,
}

impl Meter<'_> {
    /// The tokens one envelope adds to the payload: its serialized string and
    /// the comma that separates it.
    fn cost(&mut self, piece: &Piece) -> Result<u64, DeliveryError> {
        let key = (
            piece.memory.id,
            piece.range.start,
            piece.range.end,
            piece.cut,
        );
        if let Some(cost) = self.costs.get(&key) {
            return Ok(*cost);
        }
        let framed = frame_evidence([piece.input()?])?;
        let text = framed
            .memories
            .first()
            .map(|envelope| envelope.text.as_str())
            .unwrap_or_default();
        let cost = self.counter.count(&serde_json::to_string(text)?) + 1;
        self.costs.insert(key, cost);
        Ok(cost)
    }

    /// The exact count of the serialized payload of `pieces`.
    fn payload(&self, pieces: &[&Piece]) -> Result<u64, DeliveryError> {
        let inputs = pieces
            .iter()
            .map(|piece| piece.input())
            .collect::<Result<Vec<_>, _>>()?;
        Ok(self
            .counter
            .count(&serde_json::to_string(&frame_evidence(inputs)?)?))
    }
}

/// The evidence stage over one store (spec 025).
#[derive(Debug)]
pub struct Delivery {
    store: StoreHandle,
    config: DeliveryConfig,
}

fn whole(memory: &Memory) -> ByteRange {
    ByteRange {
        start: 0,
        end: memory.body.text.len() as u64,
    }
}

/// The winning chunk's range when it is a usable range of the body, else the
/// whole body (D-9).
fn usable_chunk(recalled: &RecalledMemory, memory: &Memory) -> ByteRange {
    recalled
        .chunk
        .filter(|range| {
            let (Ok(start), Ok(end)) = (usize::try_from(range.start), usize::try_from(range.end))
            else {
                return false;
            };
            memory.body.text.get(start..end).is_some()
        })
        .unwrap_or_else(|| whole(memory))
}

/// The largest cut of `text` that ends at a sentence or line boundary within
/// its first `limit` bytes, or `limit` when it has none (B-6 step 1).
fn boundary(text: &str, limit: usize) -> usize {
    let head = text.get(..limit).unwrap_or_default();
    let mut best = 0;
    let mut chars = head.char_indices().peekable();
    while let Some((at, c)) = chars.next() {
        let end = at + c.len_utf8();
        let sentence = matches!(c, '.' | '!' | '?')
            && chars.peek().is_none_or(|(_, next)| next.is_whitespace());
        if sentence || c == '\n' {
            best = end;
        }
    }
    if best == 0 { limit } else { best }
}

impl Delivery {
    /// The stage over `store`.
    #[must_use]
    pub const fn new(store: StoreHandle, config: DeliveryConfig) -> Self {
        Self { store, config }
    }

    /// Deliver `result` under `request` in `scope` (B-1 to B-13).
    ///
    /// Returns `None` when the request does not ask for delivery (B-3): no
    /// budget, default or otherwise, applies to it.
    ///
    /// # Errors
    ///
    /// [`DeliveryError::BudgetBelowMinimum`] when the budget cannot hold one
    /// cut envelope (B-7); store and framing errors.
    pub async fn deliver(
        &self,
        scope: &Scope,
        result: &RecallResult,
        request: &DeliveryRequest,
    ) -> Result<Option<Delivered>, DeliveryError> {
        if !request.is_requested() {
            return Ok(None);
        }
        let (counter, fallback_from) = match self.config.counter.ready() {
            Ok(()) => (Arc::clone(&self.config.counter), None),
            Err(CounterUnavailable(why)) => (
                Arc::new(Utf8Bytes) as Arc<dyn TokenCounter>,
                Some(format!(
                    "{}@{}: {why}",
                    self.config.counter.id(),
                    self.config.counter.version()
                )),
            ),
        };
        let asked = request.budget.unwrap_or(self.config.default_budget);
        let budget = asked.min(self.config.max_budget);
        let mut meter = Meter {
            counter: counter.as_ref(),
            costs: BTreeMap::new(),
        };
        let base = counter.count(&serde_json::to_string(&frame_memories([])?)?);
        let mut walk = Walk {
            delivery: self,
            scope,
            result,
            request,
            budget,
            base,
            accepted: Vec::new(),
            carried: BTreeMap::new(),
            covered: Vec::new(),
            dropped: Vec::new(),
            revision: None,
        };
        walk.run(&mut meter).await?;
        let (payload, report) = walk.finish(&mut meter, fallback_from)?;
        Ok(Some(Delivered {
            payload,
            report: DeliveryReport {
                budget_clamped: asked > self.config.max_budget,
                ..report
            },
        }))
    }
}

/// The state of one packing pass.
struct Walk<'a> {
    delivery: &'a Delivery,
    scope: &'a Scope,
    result: &'a RecallResult,
    request: &'a DeliveryRequest,
    budget: u64,
    base: u64,
    accepted: Vec<Accepted>,
    /// Each memory in the payload and the index in `accepted` that carries it.
    carried: BTreeMap<MemoryId, usize>,
    /// A covered result: its rank, memory, and the carrying index.
    covered: Vec<(u32, MemoryId, usize)>,
    dropped: Vec<DroppedReport>,
    revision: Option<Option<u32>>,
}

impl Walk<'_> {
    async fn run(&mut self, meter: &mut Meter<'_>) -> Result<(), DeliveryError> {
        let results = &self.result.memories;
        let mut used = self.base;
        for (position, recalled) in results.iter().enumerate() {
            if let Some(carrier) = self.carried.get(&recalled.memory_id) {
                self.covered
                    .push((recalled.rank, recalled.memory_id, *carrier));
                continue;
            }
            let Some(loaded) = self.load(recalled).await? else {
                self.dropped.push(DroppedReport {
                    memory_id: recalled.memory_id,
                    reason: DROPPED_UNAVAILABLE,
                });
                continue;
            };
            let first = self.accepted.is_empty();
            if first {
                self.check_minimum(&loaded, meter)?;
            }
            let conversational = loaded.hit_turn.is_some();
            let requested = match self.request.requested() {
                EvidenceUnit::Auto if conversational => EvidenceUnit::Record,
                EvidenceUnit::Auto => EvidenceUnit::Chunk,
                unit => unit,
            };
            let mut chosen = None;
            for rung in self.ladder(requested, conversational) {
                let (added, shared) = self.pieces(&loaded, rung);
                if self.fits(meter, used, &added)? {
                    chosen = Some(Accepted {
                        memory_id: recalled.memory_id,
                        rank: recalled.rank,
                        requested,
                        delivered: rung,
                        conversational,
                        pieces: added,
                        shared,
                        cut: false,
                    });
                    break;
                }
            }
            let accepted = match chosen {
                Some(accepted) => accepted,
                None if first => {
                    let budget = self.budget;
                    let piece = Self::cut_prefix(
                        &Self::piece(&loaded.hit, loaded.chunk),
                        meter,
                        |meter, piece| Ok(meter.payload(&[piece])? <= budget),
                    )?;
                    Accepted {
                        memory_id: recalled.memory_id,
                        rank: recalled.rank,
                        requested,
                        delivered: EvidenceUnit::Chunk,
                        conversational,
                        cut: piece.cut,
                        pieces: vec![piece],
                        shared: Vec::new(),
                    }
                }
                None => {
                    // B-6 step 3: the first result that does not fit even as
                    // its chunk is dropped, and so is every one after it.
                    for later in results.iter().skip(position) {
                        self.dropped.push(DroppedReport {
                            memory_id: later.memory_id,
                            reason: DROPPED_BUDGET,
                        });
                    }
                    return Ok(());
                }
            };
            let index = self.accepted.len();
            for piece in &accepted.pieces {
                self.carried.insert(piece.memory.id, index);
            }
            self.accepted.push(accepted);
            used = self.recount(meter)?;
        }
        Ok(())
    }

    /// Whether the payload so far plus `added` is within the budget.
    ///
    /// Each envelope is estimated at its serialized string plus one token
    /// for its separator, which is at most two tokens more than the exact
    /// marginal count for the shipped counters. An estimate that fits is
    /// accepted, one that is over by more than that slack is refused, and
    /// anything between is decided by recounting the serialized payload, so a
    /// budget that exactly holds the payload accepts it (FR-002).
    fn fits(
        &self,
        meter: &mut Meter<'_>,
        used: u64,
        added: &[Piece],
    ) -> Result<bool, DeliveryError> {
        let slack = 2 * added.len() as u64;
        let mut estimate = 0_u64;
        for piece in added {
            estimate = estimate.saturating_add(meter.cost(piece)?);
            if used.saturating_add(estimate) > self.budget.saturating_add(slack) {
                return Ok(false);
            }
        }
        if used.saturating_add(estimate) <= self.budget {
            return Ok(true);
        }
        let mut pieces: Vec<&Piece> = self
            .accepted
            .iter()
            .flat_map(|accepted| accepted.pieces.iter())
            .collect();
        pieces.extend(added);
        Ok(meter.payload(&pieces)? <= self.budget)
    }

    /// B-7: the budget must hold the framing, one envelope's labels, a cut
    /// marker, and one character of the rank-one body.
    fn check_minimum(&self, loaded: &Loaded, meter: &mut Meter<'_>) -> Result<(), DeliveryError> {
        let probe = self.probe(loaded);
        let minimum = meter.payload(&[&probe])?;
        if self.budget < minimum {
            return Err(DeliveryError::BudgetBelowMinimum {
                budget: self.budget,
                minimum,
            });
        }
        Ok(())
    }

    /// The smallest envelope this result can be delivered as: its chunk cut
    /// after its first character, or whole when it has no text.
    fn probe(&self, loaded: &Loaded) -> Piece {
        let text = &loaded.hit.memory.body.text;
        let start = usize::try_from(loaded.chunk.start).unwrap_or(0);
        let end = usize::try_from(loaded.chunk.end).unwrap_or(0);
        let chunk = text.get(start..end).unwrap_or_default();
        let first = chunk.chars().next().map_or(0, char::len_utf8);
        let mut piece = Self::piece(&loaded.hit, loaded.chunk);
        if first > 0 && first < chunk.len() {
            piece.range.end = loaded.chunk.start + first as u64;
            piece.cut = true;
        }
        piece
    }

    fn piece(item: &Item, range: ByteRange) -> Piece {
        Piece {
            memory: item.memory.clone(),
            erased: item.erased,
            range,
            cut: false,
        }
    }

    /// `whole`, cut to the longest sentence or line prefix for which `fits`
    /// holds, or `whole` itself when it fits (B-6 steps 1 and 5). One
    /// character is the least a cut keeps; B-7 guarantees it fits.
    fn cut_prefix(
        whole: &Piece,
        meter: &mut Meter<'_>,
        mut fits: impl FnMut(&mut Meter<'_>, &Piece) -> Result<bool, DeliveryError>,
    ) -> Result<Piece, DeliveryError> {
        if fits(meter, whole)? {
            return Ok(whole.clone());
        }
        let text = &whole.memory.body.text;
        let start = usize::try_from(whole.range.start).unwrap_or(0);
        let end = usize::try_from(whole.range.end).unwrap_or(0);
        let part = text.get(start..end).unwrap_or_default();
        let ends: Vec<usize> = part
            .char_indices()
            .map(|(at, c)| at + c.len_utf8())
            .collect();
        let with_len = |len: usize| Piece {
            range: ByteRange {
                start: whole.range.start,
                end: whole.range.start + len as u64,
            },
            cut: true,
            ..whole.clone()
        };
        // Index of the longest prefix, in characters, that fits; the whole
        // part is excluded because it did not.
        let (mut low, mut high) = (0_usize, ends.len().saturating_sub(2));
        while low < high {
            let middle = (low + high).div_ceil(2);
            let length = ends.get(middle).copied().unwrap_or(0);
            if fits(meter, &with_len(length))? {
                low = middle;
            } else {
                high = middle - 1;
            }
        }
        let Some(length) = ends.get(low).copied() else {
            return Ok(whole.clone());
        };
        Ok(with_len(boundary(part, length)))
    }

    /// B-6: the rungs from the requested unit down.
    fn ladder(&self, requested: EvidenceUnit, conversational: bool) -> Vec<EvidenceUnit> {
        let widest = match requested {
            EvidenceUnit::Window { radius } => radius,
            _ => self.request.radius(),
        };
        let mut rungs = Vec::new();
        match requested {
            EvidenceUnit::Chunk => return vec![EvidenceUnit::Chunk],
            EvidenceUnit::Record => rungs.push(EvidenceUnit::Record),
            _ => {}
        }
        rungs.extend(
            (1..=widest)
                .rev()
                .map(|radius| EvidenceUnit::Window { radius }),
        );
        if conversational {
            rungs.push(EvidenceUnit::Turn);
        }
        rungs.push(EvidenceUnit::Chunk);
        rungs
    }

    /// The envelopes `rung` adds, and the turns it shares with earlier units
    /// (B-6: every turn appears in a payload at most once).
    fn pieces(&self, loaded: &Loaded, rung: EvidenceUnit) -> (Vec<Piece>, Vec<TurnReport>) {
        let hit = &loaded.hit;
        let single = |range| (vec![Self::piece(hit, range)], Vec::new());
        let Some(hit_turn) = loaded.hit_turn else {
            return match rung {
                EvidenceUnit::Record => single(whole(&hit.memory)),
                EvidenceUnit::Window { radius } => single(Self::chunk_window(loaded, radius)),
                _ => single(loaded.chunk),
            };
        };
        let (first, last) = match rung {
            EvidenceUnit::Record => (1, u32::MAX),
            EvidenceUnit::Window { radius } => (
                hit_turn.saturating_sub(u32::from(radius)).max(1),
                hit_turn.saturating_add(u32::from(radius)),
            ),
            EvidenceUnit::Turn => return single(whole(&hit.memory)),
            _ => return single(loaded.chunk),
        };
        let mut ordered: Vec<(u32, &Item)> = loaded
            .others
            .range(first..=last)
            .map(|(turn, item)| (*turn, item))
            .collect();
        ordered.push((hit_turn, hit));
        ordered.sort_by_key(|(turn, _)| *turn);
        let mut added = Vec::new();
        let mut shared = Vec::new();
        for (turn, item) in ordered {
            if self.carried.contains_key(&item.memory.id) {
                shared.push(TurnReport {
                    memory_id: item.memory.id,
                    turn,
                });
            } else {
                added.push(Self::piece(item, whole(&item.memory)));
            }
        }
        (added, shared)
    }

    /// The winning chunk and `radius` neighbors each side, as one range.
    fn chunk_window(loaded: &Loaded, radius: u8) -> ByteRange {
        let Some(at) = loaded
            .chunks
            .iter()
            .position(|range| *range == loaded.chunk)
        else {
            return loaded.chunk;
        };
        let radius = usize::from(radius);
        let low = at.saturating_sub(radius);
        let high = (at + radius).min(loaded.chunks.len() - 1);
        let span = loaded.chunks.get(low..=high).unwrap_or_default();
        ByteRange {
            start: span.iter().map(|range| range.start).min().unwrap_or(0),
            end: span.iter().map(|range| range.end).max().unwrap_or(0),
        }
    }

    /// Read what one result's ladder can need, under the caller's scope and
    /// the query's status predicate (B-9). `None` when the memory is gone.
    async fn load(&mut self, recalled: &RecalledMemory) -> Result<Option<Loaded>, DeliveryError> {
        let store = &self.delivery.store;
        let repo = MemoryRepo::new();
        let Some(memory) = repo.get(store, self.scope, recalled.memory_id).await? else {
            return Ok(None);
        };
        let admitted = self.result.trace.filters.status_labels();
        if !admitted.contains(&memory.status.label()) {
            return Ok(None);
        }
        let chunk = usable_chunk(recalled, &memory);
        let conversation = memory.provenance.conversation.clone();
        let hit_turn = conversation.as_ref().map(|reference| reference.turn);
        let mut others = Vec::new();
        if let Some(reference) = &conversation {
            let range = match self.request.requested() {
                EvidenceUnit::Chunk => None,
                EvidenceUnit::Window { radius } => Some(TurnRange::Between {
                    first: reference.turn.saturating_sub(u32::from(radius)).max(1),
                    last: reference.turn.saturating_add(u32::from(radius)),
                }),
                _ => Some(TurnRange::All),
            };
            if let Some(range) = range {
                others = ProvenanceRepo::conversation_turns(
                    store,
                    self.scope,
                    &memory.provenance.source.system,
                    &reference.id,
                    range,
                )
                .await?;
            }
        }
        others.retain(|other| other.id != memory.id);
        let ids: Vec<MemoryId> = std::iter::once(memory.id)
            .chain(others.iter().map(|other| other.id))
            .collect();
        let erased: BTreeSet<MemoryId> = repo.origin_erased(store, self.scope, &ids).await?;
        let chunks = if conversation.is_none()
            && matches!(
                self.request.requested(),
                EvidenceUnit::Window { .. } | EvidenceUnit::Record
            ) {
            self.chunks(&memory).await?
        } else {
            Vec::new()
        };
        let others = others
            .into_iter()
            .filter_map(|other| {
                let turn = other.provenance.conversation.as_ref()?.turn;
                let erased = erased.contains(&other.id);
                Some((
                    turn,
                    Item {
                        memory: other,
                        erased,
                    },
                ))
            })
            .collect();
        let hit_erased = erased.contains(&memory.id);
        Ok(Some(Loaded {
            hit: Item {
                memory,
                erased: hit_erased,
            },
            hit_turn,
            chunk,
            chunks,
            others,
        }))
    }

    /// The chunks of `memory` under the active model revision, in order.
    async fn chunks(&mut self, memory: &Memory) -> Result<Vec<ByteRange>, DeliveryError> {
        let store = &self.delivery.store;
        let revision = match self.revision {
            Some(revision) => revision,
            None => {
                let active = ModelRegistry::active(store)
                    .await?
                    .map(|model| model.revision);
                self.revision = Some(active);
                active
            }
        };
        let Some(revision) = revision else {
            return Ok(Vec::new());
        };
        #[derive(Deserialize)]
        struct Row {
            byte_start: i64,
            byte_end: i64,
        }
        let rows: Vec<Row> = store
            .query(
                "SELECT byte_start, byte_end FROM chunk
                 WHERE scope_id = ?1 AND memory_id = ?2 AND model_revision = ?3
                 ORDER BY ordinal"
                    .to_owned(),
                vec![
                    Value::from(ScopeId::of(self.scope).as_str()),
                    Value::from(memory.id.to_string()),
                    Value::Integer(i64::from(revision)),
                ],
            )
            .await?;
        Ok(rows
            .into_iter()
            .map(|row| ByteRange {
                start: u64::try_from(row.byte_start).unwrap_or(0),
                end: u64::try_from(row.byte_end).unwrap_or(0),
            })
            .collect())
    }

    /// B-6 step 5: serialize through 019's framing, recount, and trim until
    /// the recount is within the budget, then assemble the report.
    fn finish(
        &mut self,
        meter: &mut Meter<'_>,
        fallback_from: Option<String>,
    ) -> Result<(FramedMemories, DeliveryReport), DeliveryError> {
        let mut used = self.recount(meter)?;
        while used > self.budget {
            let last_one = self.accepted.len() <= 1;
            self.trim_last(meter)?;
            used = self.recount(meter)?;
            if last_one && used > self.budget {
                return Err(DeliveryError::BudgetBelowMinimum {
                    budget: self.budget,
                    minimum: used,
                });
            }
        }
        let inputs = self
            .accepted
            .iter()
            .flat_map(|accepted| accepted.pieces.iter())
            .map(Piece::input)
            .collect::<Result<Vec<_>, _>>()?;
        let payload = frame_evidence(inputs)?;
        let mut results = Vec::new();
        for accepted in &self.accepted {
            let mut tokens = 0_u64;
            for piece in &accepted.pieces {
                tokens = tokens.saturating_add(meter.cost(piece)?);
            }
            results.push(Self::report(accepted, tokens));
        }
        for (rank, memory_id, carrier) in &self.covered {
            let Some(carrying) = self.accepted.get(*carrier) else {
                continue;
            };
            results.push(ResultReport {
                memory_id: *memory_id,
                rank: *rank,
                requested: carrying.requested,
                delivered: None,
                reason: Reason::Covered,
                byte_ranges: Vec::new(),
                turns: Self::turns(carrying),
                shared_turns: Vec::new(),
                covered_by: Some(carrying.memory_id),
                tokens: 0,
            });
        }
        results.sort_by_key(|result| result.rank);
        Ok((
            payload,
            DeliveryReport {
                counter: CounterReport {
                    id: meter.counter.id().to_owned(),
                    version: meter.counter.version(),
                    fallback_from,
                },
                budget: self.budget,
                budget_clamped: false,
                used,
                results,
                dropped: self.dropped.clone(),
            },
        ))
    }

    fn recount(&self, meter: &Meter<'_>) -> Result<u64, DeliveryError> {
        let pieces: Vec<&Piece> = self
            .accepted
            .iter()
            .flat_map(|accepted| accepted.pieces.iter())
            .collect();
        meter.payload(&pieces)
    }

    /// Cut or drop the last delivered result until the recount fits. The
    /// rank-one result is only ever cut, never dropped (B-6 step 5).
    fn trim_last(&mut self, meter: &mut Meter<'_>) -> Result<(), DeliveryError> {
        if self.accepted.len() > 1 {
            let Some(last) = self.accepted.pop() else {
                return Ok(());
            };
            let index = self.accepted.len();
            for piece in &last.pieces {
                self.carried.remove(&piece.memory.id);
            }
            self.dropped.push(DroppedReport {
                memory_id: last.memory_id,
                reason: DROPPED_BUDGET,
            });
            // A covered result whose carrier is gone is dropped with it.
            let (gone, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut self.covered)
                .into_iter()
                .partition(|(_, _, carrier)| *carrier >= index);
            self.covered = kept;
            self.dropped
                .extend(gone.into_iter().map(|(_, memory_id, _)| DroppedReport {
                    memory_id,
                    reason: DROPPED_BUDGET,
                }));
            return Ok(());
        }
        let budget = self.budget;
        let Some(only) = self.accepted.first_mut() else {
            return Ok(());
        };
        let Some(hit) = only
            .pieces
            .iter()
            .find(|piece| piece.memory.id == only.memory_id)
            .cloned()
        else {
            return Ok(());
        };
        let cut = Self::cut_prefix(&hit, meter, |meter, piece| {
            Ok(meter.payload(&[piece])? <= budget)
        })?;
        only.delivered = EvidenceUnit::Chunk;
        only.cut = true;
        only.shared.clear();
        only.pieces = vec![cut];
        Ok(())
    }

    fn turns(accepted: &Accepted) -> Vec<TurnReport> {
        if !accepted.conversational {
            return Vec::new();
        }
        accepted
            .pieces
            .iter()
            .filter_map(|piece| {
                piece
                    .memory
                    .provenance
                    .conversation
                    .as_ref()
                    .map(|reference| TurnReport {
                        memory_id: piece.memory.id,
                        turn: reference.turn,
                    })
            })
            .collect()
    }

    fn report(accepted: &Accepted, tokens: u64) -> ResultReport {
        let unit_is_conversation = accepted.conversational
            && matches!(
                accepted.delivered,
                EvidenceUnit::Window { .. } | EvidenceUnit::Record
            );
        let reason = if accepted.cut {
            Reason::Cut
        } else if unit_is_conversation {
            Reason::Conversation
        } else if accepted.delivered == accepted.requested {
            Reason::Fit
        } else {
            Reason::Reduced
        };
        ResultReport {
            memory_id: accepted.memory_id,
            rank: accepted.rank,
            requested: accepted.requested,
            delivered: Some(accepted.delivered),
            reason,
            byte_ranges: accepted.pieces.iter().map(|piece| piece.range).collect(),
            turns: Self::turns(accepted),
            shared_turns: accepted.shared.clone(),
            covered_by: None,
            tokens,
        }
    }
}
