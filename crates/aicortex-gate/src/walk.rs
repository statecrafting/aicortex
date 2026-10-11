//! The closed walk (spec 057): both of the gate's questions are sequenced by
//! action-gate's closed evaluation mode (action-gate spec 004) rather than by
//! a hand-written chain of early returns.
//!
//! Each rule of 013 and 051 is a [`Step`]: a stable id and a pure function
//! from the evaluation's subject to [`Judged`]. [`walk`] registers every step
//! as a required check of one closed `action_gate_core::Gate`, in the order
//! the owning spec documents, and asks it once. Three properties then belong
//! to the gate rather than to this crate's control flow (057 B-2):
//!
//! - **A refusal ends the walk** (004 B-4). The size ceiling stops the
//!   detectors from scanning an unbounded body (013 B-6).
//! - **A hold does not end the walk, and any refusal outranks it** (004 B-5,
//!   B-7). Origin's quarantine cannot store a body a later detector refuses,
//!   wherever origin is registered (013 B-4, B-7).
//! - **A required step that is missing refuses** (004 B-3). There is no
//!   assembly of the walk that drops the detectors and still admits.
//!
//! What stays here is the adapter of 004 section 6.2: a `Decision` carries no
//! payload (action-gate 001 D-4), so the deciding step is named by the
//! decision's `check_ids` and judged once more on the same subject to recover
//! its reason with its counts, offsets and detector. Every step is pure, so
//! the second judgement is the first one (013 B-10).

use std::sync::Arc;

use action_gate_core::{ActionContext, Check, Decision, Gate as ClosedGate, Outcome, closed};

/// The action every walk evaluates. The steps read their subject, never the
/// context, so the context carries nothing but this name.
const ACTION: &str = "aicortex.gate.walk";

/// What one step makes of the subject.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Judged<R> {
    /// Nothing wrong: the step affirms, and the walk goes on.
    Pass,
    /// Admit only out of sight (a quarantine) or not yet (a hold). The walk
    /// goes on, and a later refusal outranks it.
    Hold(R),
    /// Refuse. The walk ends here.
    Fault(R),
}

/// One rule of the walk: its stable id and its judgement.
pub(crate) struct Step<S, R> {
    /// The check id, which is also the required id.
    pub id: &'static str,
    /// The rule. Pure: no clock, no randomness, no I/O.
    pub judge: fn(&S) -> Judged<R>,
}

impl<S, R> Clone for Step<S, R> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<S, R> Copy for Step<S, R> {}

/// A step registered on the closed gate, holding its share of the subject.
struct StepCheck<S, R> {
    step: Step<S, R>,
    subject: Arc<S>,
}

impl<S, R> Check for StepCheck<S, R>
where
    S: Send + Sync,
{
    fn id(&self) -> &str {
        self.step.id
    }

    fn evaluate(&self, _ctx: &ActionContext) -> Option<Decision> {
        let id = self.step.id;
        Some(match (self.step.judge)(&self.subject) {
            Judged::Pass => Decision {
                outcome: Outcome::Allow,
                reason: format!("aicortex:allow:{id}:clean"),
                check_ids: vec![id.to_owned()],
                blocking: false,
            },
            Judged::Hold(_) => Decision::degrade(format!("aicortex:degrade:{id}"), vec![id.into()]),
            Judged::Fault(_) => {
                Decision::deny(format!("aicortex:deny:{id}"), vec![id.into()]).blocking()
            }
        })
    }
}

/// The outcome of a walk, with the deciding step's reason recovered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Walked<R> {
    /// Every step passed.
    Pass,
    /// No step refused, and one held: the first hold's reason.
    Hold(R),
    /// A step refused, or the walk itself did: the first refusal's reason.
    Fault(R),
}

/// Run `steps` over `subject` as one closed gate requiring every id in
/// `required`.
///
/// `walk_fault` names a refusal the gate makes itself (004 B-3 to B-5) in
/// the caller's reason type. With `required` equal to the ids of `steps`,
/// as both of the crate's walks pass it, none of those can occur: every
/// requirement is registered and every step decides. They are mapped rather
/// than assumed away, and each one refuses.
pub(crate) fn walk<S, R>(
    subject: S,
    steps: &[Step<S, R>],
    required: &[&'static str],
    walk_fault: fn(&'static str) -> R,
) -> Walked<R>
where
    S: Send + Sync + 'static,
    R: 'static,
{
    let subject = Arc::new(subject);
    let gate = steps
        .iter()
        .fold(
            ClosedGate::builder()
                .closed()
                .require_all(required.iter().copied()),
            |builder, step| {
                builder.check(StepCheck {
                    step: *step,
                    subject: Arc::clone(&subject),
                })
            },
        )
        .build();
    let decision = gate.evaluate(&ActionContext::new(ACTION));
    if decision.outcome == Outcome::Allow {
        return Walked::Pass;
    }
    let decided = decision
        .check_ids
        .first()
        .and_then(|id| steps.iter().find(|step| step.id == id.as_str()))
        .map(|step| (step.judge)(&subject));
    match (decision.outcome, decided) {
        (Outcome::Degrade, Some(Judged::Hold(reason))) => Walked::Hold(reason),
        (Outcome::Deny, Some(Judged::Fault(reason))) => Walked::Fault(reason),
        _ => Walked::Fault(walk_fault(walk_code(&decision.reason))),
    }
}

/// The stable code of a refusal the gate made itself, or
/// [`WALK_INCONSISTENT`] for a decision no step's second judgement confirms.
fn walk_code(reason: &str) -> &'static str {
    [
        closed::REQUIRED_UNREGISTERED,
        closed::REQUIRED_UNDECIDED,
        closed::NO_CHECK_DECIDED,
    ]
    .into_iter()
    .find(|code| *code == reason)
    .unwrap_or(WALK_INCONSISTENT)
}

/// The code of a walk whose deciding step judged differently the second
/// time. Unreachable while every step is pure; a refusal if it ever is not.
pub(crate) const WALK_INCONSISTENT: &str = "aicortex:deny:walk:inconsistent";

/// The check ids of `steps`, in registration order.
#[cfg(test)]
pub(crate) fn step_ids<S, R>(steps: &[Step<S, R>]) -> Vec<&'static str> {
    steps.iter().map(|step| step.id).collect()
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]
mod tests {
    use super::*;

    fn pass(_: &u8) -> Judged<&'static str> {
        Judged::Pass
    }
    fn hold(_: &u8) -> Judged<&'static str> {
        Judged::Hold("held")
    }
    fn fault(_: &u8) -> Judged<&'static str> {
        Judged::Fault("faulted")
    }
    fn code(code: &'static str) -> &'static str {
        code
    }

    const PASS: Step<u8, &'static str> = Step {
        id: "pass",
        judge: pass,
    };
    const HOLD: Step<u8, &'static str> = Step {
        id: "hold",
        judge: hold,
    };
    const FAULT: Step<u8, &'static str> = Step {
        id: "fault",
        judge: fault,
    };

    fn run(steps: &[Step<u8, &'static str>], required: &[&'static str]) -> Walked<&'static str> {
        walk(0, steps, required, code)
    }

    #[test]
    fn a_hold_is_outranked_by_a_later_fault_and_outranks_a_pass() {
        let all = ["pass", "hold", "fault"];
        assert_eq!(run(&[HOLD, FAULT], &all[1..]), Walked::Fault("faulted"));
        assert_eq!(run(&[PASS, HOLD, PASS], &all[..2]), Walked::Hold("held"));
        assert_eq!(run(&[PASS], &all[..1]), Walked::Pass);
    }

    #[test]
    fn the_walks_own_refusals_are_mapped_to_their_stable_codes() {
        assert_eq!(
            run(&[PASS], &["pass", "missing"]),
            Walked::Fault(closed::REQUIRED_UNREGISTERED)
        );
        assert_eq!(run(&[], &[]), Walked::Fault(closed::NO_CHECK_DECIDED));
        assert_eq!(walk_code("aicortex:deny:elsewhere"), WALK_INCONSISTENT);
        assert_eq!(step_ids(&[PASS, FAULT]), ["pass", "fault"]);
    }
}
