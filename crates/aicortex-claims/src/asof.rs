//! Explicit bounds for both transaction time and valid time.

use aicortex_types::{Interval, TimePoint};
use rahi_types::UnixSeconds;
use serde::{Deserialize, Serialize};

/// Upper bound on what the store knew.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum TxBound {
    /// Include transaction sequences through this value.
    Sequence(u64),
    /// Include records committed at or before this instant.
    RecordedAt(UnixSeconds),
}

/// Point or interval being queried on the valid-time axis.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum ValidBound {
    /// One explicit point.
    Instant(TimePoint),
    /// A whole interval that must be covered.
    Interval(Interval),
}

/// Required bounds on both axes. There is deliberately no `Default`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AsOf {
    /// Transaction-time knowledge bound.
    pub knowledge: TxBound,
    /// Valid-time bound.
    pub valid: ValidBound,
}
