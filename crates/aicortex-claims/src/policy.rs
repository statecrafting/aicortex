//! Versioned projection policy.

use serde::{Deserialize, Serialize};

/// Time-zone database compiled into this build.
pub const SUPPORTED_TZDB_VERSION: &str = chrono_tz::IANA_TZDB_VERSION;

/// Every input that may change valid-time projection semantics.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectionPolicy {
    /// Operator-assigned immutable policy version.
    pub version: String,
    /// Widening on either side of a floating wall time.
    pub floating_window_seconds: u32,
    /// IANA database identity required by this policy.
    pub time_zone_database: String,
    /// Whether possible coverage is eligible to win.
    pub possible_counts: bool,
}

impl ProjectionPolicy {
    /// The first policy: fourteen hours and this build's database identity.
    #[must_use]
    pub fn v1() -> Self {
        Self {
            version: "1".to_owned(),
            floating_window_seconds: 14 * 60 * 60,
            time_zone_database: SUPPORTED_TZDB_VERSION.to_owned(),
            possible_counts: true,
        }
    }

    /// Deterministic digest of the complete policy document.
    #[must_use]
    pub fn digest(&self) -> String {
        let bytes = serde_json::to_vec(self).unwrap_or_default();
        format!("blake3:{}", blake3::hash(&bytes).to_hex())
    }
}
