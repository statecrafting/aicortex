//! The write gate (spec 013): the one thing everything entering the store
//! passes through, and a pure function.
//!
//! A memory store is a place people paste things, and some of those things
//! are credentials. Once a credential is stored it is also embedded, indexed,
//! backed up and returned by recall, and every one of those is a copy. The
//! only safe moment is before the write, which is why
//! [`Gate::evaluate`] runs before a transaction opens rather than inside one.
//!
//! Four properties hold here, and each is a mechanism rather than a habit.
//!
//! - **A credential is refused, not redacted (B-3).** There is no masking
//!   function in this crate and no verdict that stores a mangled copy. A
//!   redaction would still mean the value was transmitted to this process and
//!   written to whatever log the request path keeps, and a stored placeholder
//!   is an invitation to a later feature that "recovers" the original.
//! - **No write happens without a verdict (B-1).** [`Admitted`] has a private
//!   field and no public constructor, `aicortex_store::MemoryRepo::insert`
//!   takes one, and `testdata`'s compile-fail cases assert that the other
//!   ways in do not typecheck.
//! - **A refusal carries no content (B-3, FR-002).** [`Reason`] is a closed
//!   enum of codes, counts, offsets and detector names.
//! - **The same candidate always yields the same verdict (B-10).** No clock,
//!   no randomness, no network, no float arithmetic. The identifier and the
//!   timestamps come in on the [`Candidate`]; the entropy detector is
//!   fixed-point integers. That is what makes `testdata/corpus/` a regression
//!   suite rather than a sample.
//!
//! Spec 051 adds a second question in the same crate and on the same terms:
//! [`Gate::evaluate_claim`] judges a claim proposal, and only its
//! [`ClaimVerdict::Admit`] yields an [`AdmittedClaim`] ([`claim_gate`]).
//!
//! # Order of evaluation
//!
//! [`Gate::evaluate`] applies the rules in this order, and the order is part
//! of the contract:
//!
//! 1. Normalize ([`normalize`], B-8), so every later step reads the string a
//!    human would see rather than the one the bytes spell.
//! 2. Empty after normalization (B-6).
//! 3. The text ceiling (B-6), before any scan: scanning an unbounded body is
//!    the denial of service the ceiling exists to prevent.
//! 4. The media ceilings (B-6).
//! 5. Deployment policy (B-2, `PolicyDenied`).
//! 6. The detectors (B-4) **before** the origin check (B-7), because a
//!    quarantine stores a row. A gate that checked origin first would store
//!    secrets whenever the origin was merely asserted.
//! 7. Origin (B-7): established admits, asserted quarantines, unestablished
//!    refuses.
//!
//! Steps 2 to 7 are the required checks of one closed action-gate gate
//! (`walk`, spec 057), registered in this order under the ids of
//! [`CAPTURE_STEPS`]. A refusal ends the walk, so the ceiling still stops the
//! scan; a quarantine does not, so a later refusal outranks it wherever origin
//! is registered; and a missing step refuses rather than admits.
//!
//! # Position
//!
//! ```no_run
//! # use aicortex_gate::{Candidate, Gate, Origin, Verdict};
//! # use aicortex_store::EmbeddingTarget;
//! # use rahi_store::{Envelope, TxnBuilder};
//! # fn stage(
//! #     gate: &Gate,
//! #     candidate: &Candidate,
//! #     embedding: &EmbeddingTarget,
//! #     work: &Envelope,
//! # ) -> Result<(), rahi_types::Error> {
//! match gate.evaluate(candidate) {
//!     Verdict::Admit(admitted) | Verdict::Quarantine(admitted, _) => {
//!         let mut txn = TxnBuilder::new();
//!         let provenance = admitted.memory().provenance.clone();
//!         aicortex_store::MemoryRepo::new().insert(
//!             &mut txn,
//!             &admitted,
//!             &provenance,
//!             embedding,
//!             work,
//!         )?;
//!     }
//!     Verdict::Refuse(reason) => {
//!         // Nothing is staged. The caller appends the Decision of B-9.
//!         let _ = reason.code();
//!     }
//! }
//! # Ok(())
//! # }
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod candidate;
pub mod claim_gate;
pub mod limits;
pub mod normalize;
pub mod rules;
pub mod secrets;
pub mod verdict;
mod walk;

use aicortex_types::{
    AdmissionOverride, DecisionRef, Memory, MemoryBody, MemoryId, SourceSystem, Status, TrustClass,
};
use rahi_types::Sub;

use crate::walk::{Judged, Step, Walked};

pub use candidate::{Candidate, Origin};
pub use claim_gate::{
    AdmissionPolicy, AdmittedClaim, CLAIM_STEPS, Ceiling, ClaimContext, ClaimField, ClaimReason,
    ClaimVerdict, INITIAL_POLICY_ID, KIND_CLAIM_BATCH, KIND_CLAIM_CORRECTION, KIND_CLAIM_HOLD,
    KIND_CLAIM_REFUSE, KIND_POLICY_CHANGE, PolicyDocument, PolicyError, PolicyRef, Qualification,
    SeedAcceptance, SourceState, TargetFacts, batch_entry, ceiling_of, correction_entry,
    policy_entry, proposal_entry,
};
pub use limits::{
    DEFAULT_MAX_BODY_BYTES, DEFAULT_MAX_MEDIA_REFS, DEFAULT_MEDIA_TOP_LEVELS, Limits,
};
pub use rules::{DetectorId, EntropyRule, RuleSet, SecretRules};
pub use secrets::Finding;
pub use verdict::{
    Admitted, DigestRef, KIND_OVERRIDE, KIND_QUARANTINE, KIND_REFUSE, LedgerEntry, MediaFault,
    Reason, Verdict, ledger_entry,
};

/// What can go wrong configuring or overriding the gate.
///
/// Deliberately not the same type as a [`Reason`]. A `Reason` is what the
/// gate says about a candidate; a `GateError` is what it says about the
/// caller's own request, and conflating the two is how a configuration
/// mistake ends up ledgered as a content refusal.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum GateError {
    /// A configuration tried to raise a ceiling. B-6 allows narrowing only.
    CeilingRaised {
        /// Which ceiling.
        ceiling: &'static str,
        /// What it is now.
        current: usize,
        /// What was asked for.
        asked: usize,
    },
    /// A configuration tried to add a media type the shipped rules refuse.
    MediaTypeWidened {
        /// The top-level type that was added.
        top_level: String,
    },
    /// An override named another candidate, or a reason code the gate did not
    /// produce for this one (B-5). An override is per candidate and per
    /// refusal; it is not a switch.
    OverrideDoesNotApply {
        /// The code the operator named.
        named: String,
        /// What the gate actually answered, or `None` when it admitted.
        actual: Option<String>,
    },
    /// An override carried no justification. B-5 requires one.
    OverrideUnjustified,
}

impl core::fmt::Display for GateError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::CeilingRaised {
                ceiling,
                current,
                asked,
            } => write!(
                f,
                "{ceiling} is {current} and may only be narrowed; {asked} was asked for"
            ),
            Self::MediaTypeWidened { top_level } => {
                write!(f, "the media type {top_level} is not one the gate admits")
            }
            Self::OverrideDoesNotApply { named, actual } => write!(
                f,
                "the override names {named} but the gate answered {}",
                actual.as_deref().unwrap_or("admit")
            ),
            Self::OverrideUnjustified => {
                f.write_str("an override without a justification is not an override")
            }
        }
    }
}

impl core::error::Error for GateError {}

/// An operator's decision to admit one candidate the gate refused (B-5).
///
/// Per candidate literally: the memory id the override names is checked
/// against the candidate offered, so one override admits one candidate and
/// cannot be carried to the next one. That is what makes B-5 an exception
/// rather than a switch, and FR-003 asserts it.
///
/// There is no configuration flag anywhere in this crate that disables the
/// gate, and no constructor of this type that omits a field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Override {
    memory: MemoryId,
    reason_code: String,
    justification: String,
    by: Sub,
    decision: DecisionRef,
}

impl Override {
    /// Record an operator's override.
    ///
    /// # Errors
    ///
    /// [`GateError::OverrideUnjustified`] when `justification` is blank.
    pub fn new(
        memory: MemoryId,
        reason_code: impl Into<String>,
        justification: impl Into<String>,
        by: Sub,
        decision: DecisionRef,
    ) -> Result<Self, GateError> {
        let justification = justification.into();
        if justification.trim().is_empty() {
            return Err(GateError::OverrideUnjustified);
        }
        Ok(Self {
            memory,
            reason_code: reason_code.into(),
            justification,
            by,
            decision,
        })
    }

    /// The one candidate this override admits.
    #[must_use]
    pub const fn memory(&self) -> MemoryId {
        self.memory
    }

    /// The refusal code this override answers.
    #[must_use]
    pub fn reason_code(&self) -> &str {
        &self.reason_code
    }

    /// Why the operator overrode it.
    #[must_use]
    pub fn justification(&self) -> &str {
        &self.justification
    }

    /// The human who decided.
    #[must_use]
    pub const fn by(&self) -> &Sub {
        &self.by
    }

    /// The ledger decision that records the act.
    #[must_use]
    pub const fn decision(&self) -> &DecisionRef {
        &self.decision
    }
}

/// The write gate.
///
/// Holds a [`RuleSet`] and nothing else: no handle, no cache, no counter. Two
/// gates with the same rules are the same function.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Gate {
    rules: RuleSet,
}

impl Gate {
    /// The gate with the shipped rules.
    #[must_use]
    pub fn standard() -> Self {
        Self::default()
    }

    /// The gate with a deployment's rules.
    #[must_use]
    pub const fn new(rules: RuleSet) -> Self {
        Self { rules }
    }

    /// The rules this gate consults.
    #[must_use]
    pub const fn rules(&self) -> &RuleSet {
        &self.rules
    }

    /// Judge a candidate (B-1).
    ///
    /// Pure: no clock, no randomness, no I/O. The same candidate and rule set
    /// always yield the same verdict (B-10).
    #[must_use]
    pub fn evaluate(&self, candidate: &Candidate) -> Verdict {
        match self.walk_capture(candidate, &CAPTURE_WALK) {
            Walked::Pass => Verdict::Admit(self.admit(candidate)),
            Walked::Hold(reason) => Verdict::Quarantine(self.quarantine(candidate), reason),
            Walked::Fault(reason) => Verdict::Refuse(reason),
        }
    }

    /// Judge a candidate an operator has overridden a refusal on (B-5).
    ///
    /// The override applies to exactly this candidate and exactly the refusal
    /// it names: the gate re-evaluates, and an override whose code does not
    /// match what the gate answered is an error rather than an admission. The
    /// memory is admitted with [`TrustClass::Assertion`], whatever the
    /// candidate claimed, and carries the override in its provenance.
    ///
    /// # Errors
    ///
    /// [`GateError::OverrideDoesNotApply`] when this override names another
    /// candidate, or when this candidate is not refused for the code the
    /// override names.
    pub fn evaluate_overridden(
        &self,
        candidate: &Candidate,
        over: &Override,
    ) -> Result<Verdict, GateError> {
        let verdict = self.evaluate(candidate);
        let actual = verdict.reason().map(|reason| reason.code().to_owned());
        let refused = matches!(verdict, Verdict::Refuse(_));
        let same_candidate = over.memory() == candidate.parts.id;
        if !same_candidate || !refused || actual.as_deref() != Some(over.reason_code()) {
            return Err(GateError::OverrideDoesNotApply {
                named: over.reason_code().to_owned(),
                actual,
            });
        }
        let mut memory = self.normalized(candidate);
        memory.trust = TrustClass::Assertion;
        memory.provenance = memory.provenance.overridden(AdmissionOverride::new(
            over.reason_code().to_owned(),
            over.by().clone(),
            over.decision().clone(),
        ));
        Ok(Verdict::Admit(Admitted::new(memory)))
    }

    /// The normalized bytes a Decision's keyed digest is taken over (B-9).
    ///
    /// The caller mints the key and computes the digest, because minting a
    /// key needs randomness and this crate has none. What the gate owns is
    /// *which bytes*: the normalized text, under the scope, so that the same
    /// body in two scopes does not produce the same material and a digest
    /// cannot be carried between them.
    ///
    /// The return value is the candidate's own content. It has exactly one
    /// legitimate destination, which is the keyed digest; it must not be
    /// logged, returned to a client, or stored.
    #[must_use]
    pub fn digest_material(&self, candidate: &Candidate) -> Vec<u8> {
        let body = normalize::body(&candidate.parts.body);
        let mut material = Vec::with_capacity(body.text.len() + 64);
        material.extend_from_slice(candidate.parts.scope.to_string().as_bytes());
        material.push(0x1f);
        material.extend_from_slice(candidate.parts.kind.label().as_bytes());
        material.push(0x1f);
        material.extend_from_slice(body.text.as_bytes());
        material
    }

    /// Walk `steps` over `candidate`, requiring every id of
    /// [`CAPTURE_STEPS`] (spec 057 B-3).
    fn walk_capture(&self, candidate: &Candidate, steps: &[CaptureStep]) -> Walked<Reason> {
        let subject = Capture {
            body: normalize::body(&candidate.parts.body),
            source: candidate.parts.provenance.source.system.clone(),
            origin: candidate.origin,
            rules: self.rules.clone(),
        };
        walk::walk(subject, steps, &CAPTURE_STEPS, |code| {
            Reason::PolicyDenied {
                policy: code.to_owned(),
            }
        })
    }

    /// The candidate as the record of 011, with its body normalized.
    fn normalized(&self, candidate: &Candidate) -> Memory {
        let _ = self;
        let mut parts = candidate.parts.clone();
        parts.body = normalize::body(&parts.body);
        Memory::new(parts)
    }

    /// The admitted form: active, normalized.
    fn admit(&self, candidate: &Candidate) -> Admitted {
        Admitted::new(self.normalized(candidate))
    }

    /// The quarantined form: normalized, and out of sight (B-7).
    fn quarantine(&self, candidate: &Candidate) -> Admitted {
        let mut memory = self.normalized(candidate);
        memory.status = Status::Quarantined;
        Admitted::new(memory)
    }
}

/// The check ids of [`Gate::evaluate`]'s walk, in registration order (spec
/// 057 B-3): every one is required, so a walk without one of them refuses.
pub const CAPTURE_STEPS: [&str; 6] = [
    "capture.empty",
    "capture.too_large",
    "capture.media",
    "capture.denied_source",
    "capture.secrets",
    "capture.origin",
];

/// What the capture walk judges: the normalized body (B-8), the source
/// system, the origin, and the rules, owned so each step can hold a share.
struct Capture {
    body: MemoryBody,
    source: SourceSystem,
    origin: Origin,
    rules: RuleSet,
}

type CaptureStep = Step<Capture, Reason>;

/// The steps of the module documentation, in its order (013 B-2 to B-7).
const CAPTURE_WALK: [CaptureStep; 6] = [
    Step {
        id: CAPTURE_STEPS[0],
        judge: empty,
    },
    Step {
        id: CAPTURE_STEPS[1],
        judge: too_large,
    },
    Step {
        id: CAPTURE_STEPS[2],
        judge: media,
    },
    Step {
        id: CAPTURE_STEPS[3],
        judge: denied_source,
    },
    Step {
        id: CAPTURE_STEPS[4],
        judge: secrets_step,
    },
    Step {
        id: CAPTURE_STEPS[5],
        judge: origin,
    },
];

/// Empty after normalization (B-6).
fn empty(capture: &Capture) -> Judged<Reason> {
    if capture.body.text.trim().is_empty() {
        Judged::Fault(Reason::EmptyAfterNormalization)
    } else {
        Judged::Pass
    }
}

/// The text ceiling (B-6), before any scan.
fn too_large(capture: &Capture) -> Judged<Reason> {
    let bytes = capture.body.text.len();
    let ceiling = capture.rules.limits.max_body_bytes;
    if bytes > ceiling {
        Judged::Fault(Reason::TooLarge { bytes, ceiling })
    } else {
        Judged::Pass
    }
}

/// The media ceilings of B-6.
fn media(capture: &Capture) -> Judged<Reason> {
    let limits = &capture.rules.limits;
    let body = &capture.body;
    let count = body.media.len();
    let ceiling = limits.max_media_refs;
    let fault = if count > ceiling {
        Some(MediaFault::TooMany { count, ceiling })
    } else {
        body.media
            .iter()
            .position(|media| !limits.admits_media(media))
            .map(|index| MediaFault::UnsupportedType { index })
    };
    fault.map_or(Judged::Pass, |fault| {
        Judged::Fault(Reason::UnsupportedMedia(fault))
    })
}

/// Deployment policy (B-2, `PolicyDenied`).
fn denied_source(capture: &Capture) -> Judged<Reason> {
    if capture.rules.denied_sources.contains(&capture.source) {
        Judged::Fault(Reason::PolicyDenied {
            policy: format!("denied-source:{}", capture.source),
        })
    } else {
        Judged::Pass
    }
}

/// The detectors of B-4, over the text and then the title.
///
/// The title is scanned too, and its offsets are reported against it: a
/// credential pasted into a note's title is a credential.
fn secrets_step(capture: &Capture) -> Judged<Reason> {
    let rules = &capture.rules.secrets;
    let in_text = secrets::scan(&capture.body.text, rules);
    let in_title = || {
        capture
            .body
            .title
            .as_deref()
            .and_then(|title| secrets::scan(title, rules))
    };
    in_text.or_else(in_title).map_or(Judged::Pass, |finding| {
        Judged::Fault(Reason::SecretDetected {
            detector: finding.detector,
            offset: finding.offset,
        })
    })
}

/// Origin (B-7): established admits, asserted quarantines, unestablished
/// refuses.
fn origin(capture: &Capture) -> Judged<Reason> {
    let origin = capture.origin;
    if origin.is_established() {
        Judged::Pass
    } else if origin.is_assertable() {
        Judged::Hold(Reason::OriginUnestablished { origin })
    } else {
        Judged::Fault(Reason::OriginUnestablished { origin })
    }
}

/// Spec 057's three properties of the walk, over the production steps.
#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]
mod walk_properties {
    use std::cell::Cell;

    use aicortex_types::{
        Actor, ActorId, Importance, MemoryKind, MemoryParts, Provenance, Scope, SourceRef,
    };
    use rahi_types::UnixSeconds;

    use super::*;

    const SECRET: &str = "ghp_9wQk2LmXr4Tb8Zc1Nd6Vf3Hs5Jg7Pa0Ye2Uu";

    fn candidate(text: String, origin: Origin) -> Candidate {
        let at = UnixSeconds::new(1_800_000_000);
        Candidate::new(
            MemoryParts {
                id: MemoryId::now_v7(),
                scope: Scope::personal(Sub::new("sub-walk")),
                kind: MemoryKind::Observation,
                body: MemoryBody::text(text),
                actor: Actor::human(ActorId::new("walker").unwrap()),
                provenance: Provenance::captured(
                    SourceRef::new(SourceSystem::new("walk").unwrap()),
                    at,
                    at,
                ),
                trust: TrustClass::Assertion,
                importance: Importance::at(at).unwrap(),
                created: at,
            },
            origin,
        )
    }

    thread_local! {
        static SCANS: Cell<usize> = const { Cell::new(0) };
    }

    /// The production detectors, counting how often they run.
    fn counted_secrets(capture: &Capture) -> Judged<Reason> {
        SCANS.with(|scans| scans.set(scans.get() + 1));
        secrets_step(capture)
    }

    fn with_step(id: &'static str, judge: fn(&Capture) -> Judged<Reason>) -> Vec<CaptureStep> {
        CAPTURE_WALK
            .iter()
            .map(|step| {
                if step.id == id {
                    Step { id, judge }
                } else {
                    *step
                }
            })
            .collect()
    }

    #[test]
    fn the_walk_registers_every_documented_step_in_order() {
        assert_eq!(walk::step_ids(&CAPTURE_WALK), CAPTURE_STEPS);
    }

    /// B-6 via action-gate 004 B-4: the ceiling's refusal ends the walk, so
    /// an oversized body is never scanned, and an ordinary one is.
    #[test]
    fn a_refusal_ends_the_walk_so_the_ceiling_stops_the_scan() {
        let gate = Gate::standard();
        let steps = with_step(CAPTURE_STEPS[4], counted_secrets);
        let oversized = format!("{SECRET} ").repeat(2_000);
        SCANS.with(|scans| scans.set(0));
        let walked = gate.walk_capture(&candidate(oversized, Origin::Authenticated), &steps);
        assert!(
            matches!(walked, Walked::Fault(Reason::TooLarge { .. })),
            "{walked:?}"
        );
        assert_eq!(
            SCANS.with(Cell::get),
            0,
            "the detectors ran past the ceiling"
        );

        let walked = gate.walk_capture(
            &candidate(format!("key {SECRET}"), Origin::Authenticated),
            &steps,
        );
        assert!(
            matches!(
                walked,
                Walked::Fault(Reason::SecretDetected { offset: 4, .. })
            ),
            "{walked:?}"
        );
        // Once by the gate, once more to recover the deciding step's reason
        // (057 D-2).
        assert_eq!(SCANS.with(Cell::get), 2);
    }

    /// B-4 and B-7 via action-gate 004 B-5: origin's quarantine does not end
    /// the walk, so a credential is refused under an asserted origin even
    /// when origin is registered first.
    #[test]
    fn a_quarantine_never_outranks_a_refusal_wherever_origin_is_registered() {
        let gate = Gate::standard();
        let mut origin_first = CAPTURE_WALK.to_vec();
        origin_first.rotate_right(1);
        assert_eq!(origin_first[0].id, "capture.origin");
        for steps in [&CAPTURE_WALK[..], &origin_first[..]] {
            let walked =
                gate.walk_capture(&candidate(format!("key {SECRET}"), Origin::Asserted), steps);
            assert!(
                matches!(walked, Walked::Fault(Reason::SecretDetected { .. })),
                "{walked:?}"
            );
            let walked = gate.walk_capture(
                &candidate("an ordinary note".into(), Origin::Asserted),
                steps,
            );
            assert_eq!(
                walked,
                Walked::Hold(Reason::OriginUnestablished {
                    origin: Origin::Asserted
                })
            );
        }
    }

    /// B-5 via action-gate 004 B-3: every step is required, so a walk that
    /// lacks any one of them, the detectors included, refuses a candidate the
    /// full walk admits.
    #[test]
    fn a_walk_missing_a_required_step_refuses_rather_than_admits() {
        let gate = Gate::standard();
        let clean = candidate("an ordinary note".into(), Origin::Authenticated);
        assert_eq!(gate.walk_capture(&clean, &CAPTURE_WALK), Walked::Pass);
        for dropped in CAPTURE_STEPS {
            let steps: Vec<CaptureStep> = CAPTURE_WALK
                .iter()
                .filter(|step| step.id != dropped)
                .copied()
                .collect();
            assert_eq!(
                gate.walk_capture(&clean, &steps),
                Walked::Fault(Reason::PolicyDenied {
                    policy: action_gate_core::closed::REQUIRED_UNREGISTERED.to_owned()
                }),
                "dropping {dropped} did not refuse"
            );
        }
    }
}
