---
id: "050-shared-secret-detector-convergence"
title: "Converge the write gate's overlapping secret rules with action-gate's SecretsCheck without weakening either"
status: draft
kind: "feature"
domain: "memory"
created: "2026-09-24"
authors: ["Bartek Kus"]
implementation: pending
risk: high
wave: 4
depends_on:
  - "013-write-gate-and-redaction"
extends:
  - { spec: "013-write-gate-and-redaction", unit: "crates/aicortex-gate/src/rules.rs", nature: additive }
  - { spec: "013-write-gate-and-redaction", unit: "crates/aicortex-gate/src/secrets.rs", nature: additive }
  - { spec: "013-write-gate-and-redaction", unit: "crates/aicortex-gate/Cargo.toml", nature: additive }
  - { spec: "013-write-gate-and-redaction", unit: "crates/aicortex-gate/testdata/corpus/", nature: additive }
  - { spec: "010-chassis-adoption-and-workspace", unit: { kind: section, file: "Cargo.toml", anchor: "workspace.dependencies" }, nature: additive }
references:
  - { unit: { kind: file, path: "specs/013-write-gate-and-redaction/spec.md" }, role: constraint }
summary: >
  aicortex-gate carries its own private-key markers and credential-prefix
  rules (013 B-4, `rules.rs`), and action-gate-core, the family's pure
  action gate, ships a `SecretsCheck` with an overlapping pattern set. The
  owner directed that the overlap be replaced by action-gate's check. The
  two are not equivalent today: SecretsCheck reports neither the detector
  nor the offset that 013 B-3 requires, misses the PGP private-key block
  aicortex refuses, and has an assignment rule aicortex lacks. This spec
  makes the replacement conditional on no loss of coverage or evidence,
  names the upstream work in action-gate that would satisfy it, and
  imports action-gate's extra rule into aicortex's corpus in the meantime.
---

# 050: Shared secret detector convergence

## 1. Purpose

Two repositories in one family refusing credentials with two rule tables
will drift: a token shape added to one is missed by the other, and the
difference is found by the credential that got through. Constitution VI's
reasoning (consume, never fork) applies to the family's own crates as well
as to the chassis. The owner has directed that aicortex-gate's rules be
replaced by action-gate's `SecretsCheck` where they overlap (section 7).

Replacement must not weaken spec 013, which is complete and critical. The
comparison, made against the trees on 2026-09-24:

| Property | aicortex-gate (`crates/aicortex-gate/src/rules.rs`, `secrets.rs`) | action-gate-core `SecretsCheck` (`crates/core/src/checks.rs`, feature `checks-common`) |
|---|---|---|
| Result | detector id and byte offset (013 B-3, FR-002) | a boolean match, reported as the fixed reason `gate:deny:secrets:pattern_match` |
| Private-key markers | seven exact PEM markers, including `-----BEGIN PGP PRIVATE KEY BLOCK-----` | one pattern, `-----BEGIN [A-Z ]*PRIVATE KEY-----`, case-insensitive, which does not match the PGP block marker |
| `sk-` keys | prefix rules for `sk-ant-` (tail 24) and `sk-` (tail 20), base64url alphabet, case-sensitive | `sk-[a-zA-Z0-9]{20,}`, case-insensitive |
| Other prefixes | 24 prefix rules in total, a URL-credential detector, a JWT detector, and a Q16 fixed-point entropy detector (013 D-4, D-12) | none |
| Assignment rule | none | `(api_key\|secret\|token) [:=] <24+ token chars>` |
| Input | the gate-normalized body (013 B-8) | `payload_summary` and `payload_body` of an `ActionContext` |
| Dependency | none beyond the workspace | `regex` (optional feature), `action-gate-types`, `canonical-keysort-json`, `sha2`, `hex` |

On the overlap (PEM markers and `sk-` keys), replacing aicortex's rules
with `SecretsCheck` as published at 0.1.0 would lose the detector id and
the offset of every refusal, and would admit a PGP private key block.
Neither is acceptable, and the family rule against weakening security
controls to reach a result forbids it.

## 2. Territory

`rules.rs`, `secrets.rs`, the manifest, and the fixture corpus of
`aicortex-gate` (013), and, if B-3 is taken, one workspace dependency.
Nothing in action-gate: its changes are that repository's specs.

## 3. Behavior

- **B-1 (no weakening).** A convergence step is admissible only if, over
  013's committed corpus and this spec's additions, every fixture keeps its
  recorded verdict, reason code, detector id, and offset, and FR-005 of 013
  (no false positive on the benign identifiers) still holds.
- **B-2 (take the missing rule now).** action-gate's assignment rule
  (`api_key`, `secret`, or `token` assigned a long token) becomes a
  positive fixture in aicortex's corpus, and a detector row is added to
  `rules.rs` if no existing detector refuses it. A negative fixture for an
  ordinary prose sentence using the word "token" is added with it.
- **B-3 (replace only on parity).** aicortex-gate adopts action-gate's
  pattern table for the overlapping rules only when action-gate exposes,
  from a pure API with no required `regex` feature on the gate's path or
  with `regex` accepted as a workspace dependency by the owner, a match
  that carries a stable detector id and a byte span, and covers every PEM
  marker aicortex refuses. Until then the two tables stay, and a test in
  aicortex asserts that every pattern in action-gate's default set is
  refused by aicortex's gate, so drift in one direction is caught.
- **B-4 (upstream is a request, not an edit).** The parity action-gate
  needs (detector ids, spans, the PGP marker, and ideally aicortex's prefix
  table and entropy detector) is raised in action-gate as its own spec. This
  repository does not vendor, patch, or fork action-gate to get there.

## 4. Functional requirements

- **FR-001.** Every 013 fixture yields its recorded verdict, reason,
  detector, and offset after this spec lands.
- **FR-002.** A fixture of the form `api_key = <32 token chars>` is
  refused; a prose fixture using "token" is admitted.
- **FR-003.** A test feeds each string that matches one of action-gate's
  default patterns (the three regular expressions of `SecretsCheck::default`)
  through `Gate::evaluate` and asserts a refusal.

## 5. Acceptance criteria

- **AC-1.** `cargo test -p aicortex-gate --locked` passes over the whole
  corpus.

## 6. Out of scope

Changing action-gate. Adopting action-gate's `Gate`, `Check` registry, or
decision format in aicortex, which is a different and larger question
(Q-2).

## 7. Resolved decisions

- **D-1 (2026-09-24, owner direction).** aicortex-gate's private-key and
  secret marker rules are to be replaced by action-gate's `SecretsCheck`
  where they overlap. This spec reads "where they overlap" as "where
  action-gate's check is at least as strong", because the family's
  standing rule forbids weakening a security control to reach a result,
  and records the comparison of section 1 as the reason the replacement is
  conditional.

## 8. Open questions

- **Q-1 (direction of convergence).** aicortex's detector set is the
  stronger one. Should the shared table move upstream into action-gate
  (action-gate adopts aicortex's prefix rules, PEM markers, URL, JWT, and
  entropy detectors, with ids and spans), with aicortex then consuming it?
  That satisfies D-1's intent without a regression.
- **Q-2 (adopting action-gate more broadly).** Should aicortex's write gate
  become a set of action-gate `Check`s? Not proposed here: 013's verdict
  type is load-bearing for the store's type-level insert guard (013 D-5).
- **Q-3 (ordinal and wave).** Spec 002 section 5 ends wave 4 at 049. This
  housekeeping spec sits at 050 because 046 to 049 are taken; the owner
  decides whether it moves or whether 002 gains a wave.

## 9. Obligations

Declared in the spec-spine 106 grammar, to be lifted into the frontmatter
`obligations` key when the repository's spec-spine pin reaches 0.25.0
(below it, the key is a compile error, `V-002`).

```yaml
obligations:
  - { id: "I-1", kind: invariant, text: "No convergence step loses a refusal, a detector id, or an offset that 013's corpus records.", anchor: "3-behavior" }
  - { id: "R-1", kind: requirement, text: "Every string matching action-gate's default secret patterns is refused by aicortex's gate.", anchor: "3-behavior" }
```

## Verification

```verify:cli
# planned: the new fixtures and the cross-check test exist once this spec is built.
cargo test -p aicortex-gate --locked
```
