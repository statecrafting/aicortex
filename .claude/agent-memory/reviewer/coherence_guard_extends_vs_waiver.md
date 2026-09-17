---
name: coherence-guard-extends-vs-waiver
description: An extends edge with nature additive can be used to justify replacing (not just adding to) an assertion in a file owned by a complete, approved spec, without the human-authorized Spec-Drift-Waiver the rule requires; watch for this specifically in tests/cell.rs and similar cross-spec seam files.
metadata:
  type: feedback
---

Seen in spec 012 (crates/aicortex-store): the session needed
`apps/aicortex/tests/cell.rs` (owned by spec 010, `implementation: complete`)
to assert something different from what spec 010's own FR-003 text says
("finds `Aicortex::migrations()` empty, which is schema version 0"). Spec 010
FR-003 was not amended (correctly, since amending a complete spec's
acceptance is reserved to a human per
[[adversarial-prompt-refusal]]/`.claude/rules/adversarial-prompt-refusal.md`).
Instead, the session declared an `extends` edge on `tests/cell.rs` with
`nature: additive`, replaced the `is_empty()` assertion with a new one, and
recorded the reasoning only as a dated decision entry (D-n) in the
*implementing* spec (012), not as a `Spec-Drift-Waiver:` line on the spec
whose stated acceptance is now contradicted (010).

**Why this matters:** the rule's actual mechanism for "code and its owning
spec disagree, and editing the spec is not this session's call" is a
`Spec-Drift-Waiver:` line, which needs explicit human approval and which an
agent must never write on its own authority. A same-session D-n entry in the
*other* spec (the one making the change, not the one being contradicted) is
not that instrument, even when it is transparent and well-reasoned. It also
stretches `nature: additive` to cover a same-file assertion *replacement*,
not just genuinely additive lines. `make gate`, `spec-spine couple`, and
coverage all pass in this situation (registry mechanics are satisfied by the
extends edge), so this is invisible to the mechanical gate; it only shows up
by actually reading the diff to the other spec's test file and comparing it
against that spec's FR wording.

**How to apply:** whenever a diff touches a file that is `establishes`-owned
by a spec other than the one being implemented (visible via a new/edited
`extends` edge in the changed spec's frontmatter), read the target spec's FR
text for that file and diff the actual assertion, not just check that the
edge exists and the gate is green. If the edit changes what an existing
assertion checks (rather than adding an independent new check), and the
target spec is `implementation: complete`, treat it as a coherence-guard
finding even when the implementing spec's own decision log explains it
honestly. Flag it for a human `Spec-Drift-Waiver:` on the *contradicted*
spec, not just a decision entry on the *implementing* one.
