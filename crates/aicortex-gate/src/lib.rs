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
//! # Position
//!
//! ```no_run
//! # use aicortex_gate::{Candidate, Gate, Origin, Verdict};
//! # use rahi_store::{Envelope, TxnBuilder};
//! # fn stage(
//! #     gate: &Gate,
//! #     candidate: &Candidate,
//! #     work: &Envelope,
//! # ) -> Result<(), rahi_types::Error> {
//! match gate.evaluate(candidate) {
//!     Verdict::Admit(admitted) | Verdict::Quarantine(admitted, _) => {
//!         let mut txn = TxnBuilder::new();
//!         let provenance = admitted.memory().provenance.clone();
//!         aicortex_store::MemoryRepo::new().insert(&mut txn, &admitted, &provenance, work)?;
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
pub mod limits;
pub mod normalize;
pub mod rules;
pub mod secrets;
pub mod verdict;

use aicortex_types::{AdmissionOverride, DecisionRef, Memory, MemoryId, Status, TrustClass};
use rahi_types::Sub;

pub use candidate::{Candidate, Origin};
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
        match self.fault(candidate) {
            Some(reason) => Verdict::Refuse(reason),
            None if candidate.origin.is_established() => Verdict::Admit(self.admit(candidate)),
            None if candidate.origin.is_assertable() => Verdict::Quarantine(
                self.quarantine(candidate),
                Reason::OriginUnestablished {
                    origin: candidate.origin,
                },
            ),
            None => Verdict::Refuse(Reason::OriginUnestablished {
                origin: candidate.origin,
            }),
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

    /// The first thing wrong with `candidate`, in the order of the module
    /// documentation, or `None` when nothing is.
    fn fault(&self, candidate: &Candidate) -> Option<Reason> {
        let body = normalize::body(&candidate.parts.body);
        if body.text.trim().is_empty() {
            return Some(Reason::EmptyAfterNormalization);
        }
        let bytes = body.text.len();
        let ceiling = self.rules.limits.max_body_bytes;
        if bytes > ceiling {
            return Some(Reason::TooLarge { bytes, ceiling });
        }
        if let Some(fault) = self.media_fault(&body) {
            return Some(Reason::UnsupportedMedia(fault));
        }
        if self
            .rules
            .denied_sources
            .contains(&candidate.parts.provenance.source.system)
        {
            return Some(Reason::PolicyDenied {
                policy: format!("denied-source:{}", candidate.parts.provenance.source.system),
            });
        }
        self.secret_fault(&body)
    }

    /// The media ceilings of B-6.
    fn media_fault(&self, body: &aicortex_types::MemoryBody) -> Option<MediaFault> {
        let count = body.media.len();
        let ceiling = self.rules.limits.max_media_refs;
        if count > ceiling {
            return Some(MediaFault::TooMany { count, ceiling });
        }
        body.media
            .iter()
            .position(|media| !self.rules.limits.admits_media(media))
            .map(|index| MediaFault::UnsupportedType { index })
    }

    /// The detectors of B-4, over the text and then the title.
    ///
    /// The title is scanned too, and its offsets are reported against it: a
    /// credential pasted into a note's title is a credential.
    fn secret_fault(&self, body: &aicortex_types::MemoryBody) -> Option<Reason> {
        let in_text = secrets::scan(&body.text, &self.rules.secrets);
        let in_title = body
            .title
            .as_deref()
            .and_then(|title| secrets::scan(title, &self.rules.secrets));
        in_text.or(in_title).map(|finding| Reason::SecretDetected {
            detector: finding.detector,
            offset: finding.offset,
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
