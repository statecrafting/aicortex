//! Detection (spec 013 B-4), consumed from action-gate (spec 047 B-2).
//!
//! One pass over the normalized text produces at most one finding, and a
//! finding is a detector name and a byte offset. Neither the value nor a
//! masked form of it is carried anywhere: B-3 refuses rather than redacts,
//! and a redaction that reached this far would already have put the secret in
//! the error, in the log and in whatever the caller does with them. FR-002 is
//! the assertion that keeps it that way.
//!
//! The scan itself is `action_gate_core::secrets::scan`, which is pure,
//! regex-free and deterministic (047 B-4, 013 B-10). This module only runs it
//! on the text the gate has already normalized (013 B-8) and names the
//! finding in aicortex's terms.

use action_gate_core::secrets as registry;

use crate::rules::{DetectorId, SecretRules};

/// A detector's hit: which one, and where.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Finding {
    /// The detector that fired.
    pub detector: DetectorId,
    /// The byte offset into the normalized text at which the value starts.
    pub offset: usize,
}

/// The first detector to fire on `text`, or `None` when nothing does.
///
/// The earliest offset wins; at equal offsets an armour marker beats a token
/// detector, which beats the assignment rule (action-gate 001 B-6).
#[must_use]
pub fn scan(text: &str, rules: &SecretRules) -> Option<Finding> {
    registry::scan(text, rules).map(|finding| Finding {
        detector: DetectorId::from_registry(finding.detector),
        offset: finding.offset,
    })
}
