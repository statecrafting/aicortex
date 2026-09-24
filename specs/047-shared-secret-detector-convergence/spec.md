---
id: "047-shared-secret-detector-convergence"
title: "Converge secret detection upward: aicortex's detectors move into action-gate, and aicortex consumes them"
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
  aicortex-gate carries its own credential detectors (013 B-4, `rules.rs`,
  `secrets.rs`), and action-gate-core, the family's pure action gate, ships
  a weaker `SecretsCheck` with an overlapping pattern set. The owner chose
  to converge upward: aicortex's stronger detectors, with their detector
  ids, byte offsets, and full private-key marker set, move into action-gate
  under a separate upstream spec, and aicortex-gate then consumes
  action-gate's table at an exact pin instead of keeping its own. The
  consumption is admissible only at parity with 013's committed corpus, and
  aicortex keeps its own verdict, normalization, and keyed-digest ledger
  record. Until the upstream release exists this spec is blocked, not
  mocked.
---

# 047: Shared secret detector convergence

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

Consuming `SecretsCheck` as published at 0.1.0 would lose the detector id
and offset of every refusal and would admit a PGP private key block, which
the family rule against weakening security controls forbids. The owner
therefore chose the other direction (section 7, D-2): the stronger table
moves upstream, and aicortex consumes it.

## 2. Territory

`rules.rs`, `secrets.rs`, the manifest, and the fixture corpus of
`aicortex-gate` (013), and one workspace dependency on `action-gate-core`.
Nothing in action-gate: its changes are that repository's own spec, listed
as a prerequisite in section 3.3.

## 3. Behavior

### 3.1 What moves and what stays

- **B-1 (what moves).** The detector table and the detectors that read it:
  the published-prefix rules, the PEM private-key markers (all seven,
  including `-----BEGIN PGP PRIVATE KEY BLOCK-----`), the URL-credential
  detector, the JWT detector, and the fixed-point entropy detector with its
  per-run measurement (013 D-4, D-12). Each keeps its stable detector id.
- **B-2 (what stays).** Everything that makes 013 a gate rather than a
  detector: normalization before detection (013 B-8), the `Verdict`,
  `Reason`, and `Admitted` types and the store's type-level insert guard
  (013 B-1, D-5), size and origin rules, the operator override (013 B-5),
  and the keyed-digest Decision (013 B-9). aicortex-gate calls action-gate's
  detectors on the normalized text and maps a match to
  `Reason::SecretDetected` with the detector id and offset.
- **B-3 (no weakening).** Consumption is admissible only if every fixture
  in 013's committed corpus, and this spec's additions, keeps its recorded
  verdict, reason code, detector id, and offset, and 013 FR-005 (no false
  positive on the benign identifiers) still holds. A mismatch is a stop and
  a report, never a fixture edit.

### 3.2 Consumption

- **B-4 (exact pin, pure path).** `action-gate-core` is a workspace
  dependency at one exact `=x.y.z`, as the chassis is (spec 010), and the
  path aicortex-gate calls is pure: no clock, randomness, network, or
  float, so 013 B-10's determinism holds. If the upstream detectors
  require the `regex` crate, adding it to this workspace is an owner
  decision recorded here before the build.
- **B-5 (the local table goes).** Once parity is shown, `rules.rs` keeps
  only the mapping from upstream detector ids to aicortex's `DetectorId`
  and the limits that are aicortex's own; the duplicate detector code in
  `secrets.rs` is removed in the same change. Two tables do not coexist
  after this spec lands.
- **B-6 (the assignment rule arrives).** action-gate's existing assignment
  rule (`api_key`, `secret`, or `token` assigned a long token) is part of
  the consumed table, so aicortex gains it. A positive fixture for it and a
  negative fixture of ordinary prose using the word "token" join the
  corpus.

### 3.3 Prerequisite

- **B-7 (upstream first).** This spec is blocked until action-gate
  publishes a release carrying B-1's detectors with ids and byte spans. The
  work is a separate action-gate spec, a follow-up of this one; this
  repository does not vendor, patch, or fork action-gate to get there. A
  build session that finds no such release stops and reports the missing
  prerequisite rather than mocking it (AGENTS.md, "Working the backlog").

## 4. Functional requirements

- **FR-001.** Every 013 fixture yields its recorded verdict, reason,
  detector, and offset with the consumed table.
- **FR-002.** A fixture of the form `api_key = <32 token chars>` is
  refused; a prose fixture using "token" is admitted.
- **FR-003.** `aicortex-gate` has no detector implementation of its own
  after this spec lands, asserted by a test over the crate's public items.

## 5. Acceptance criteria

- **AC-1.** `cargo test -p aicortex-gate --locked` passes over the whole
  corpus.
- **AC-2.** `cargo deny check` passes with `action-gate-core` and its
  dependencies in the tree.

## 6. Out of scope

Changing action-gate, which is the upstream spec's. Adopting action-gate's
`Gate`, `Check` registry, or decision format in aicortex: 013's verdict type
is load-bearing for the store's type-level insert guard (013 D-5).

## 7. Resolved decisions

- **D-1 (2026-09-24, owner direction).** aicortex-gate's private-key and
  secret marker rules are to be replaced by action-gate's `SecretsCheck`
  where they overlap.
- **D-2 (2026-09-24, owner decision).** Recorded from the owner's answer
  to this draft's questions: "I agree with all your suggestions and
  recommendations." Secret detection converges upward. aicortex's stronger
  detectors (detector id, offset, the PGP marker) move into action-gate,
  and aicortex then consumes action-gate. This refines D-1: the
  replacement happens, and it happens after action-gate is at least as
  strong, so no refusal is lost. The action-gate change is a separate
  upstream spec.

## 8. Follow-ups and open questions

- **F-1 (upstream spec, action-gate).** A spec in action-gate that adopts
  B-1's detectors with stable ids and byte spans, keeps them pure and
  deterministic, and publishes a release aicortex can pin. Not authored
  here.
- **Q-1 (regex).** Whether `regex` is acceptable in this workspace if the
  upstream detectors need it (B-4).

## 9. Obligations

Declared in the spec-spine 106 grammar, to be lifted into the frontmatter
`obligations` key when this repository's spec-spine pin moves to 0.25.0,
which is a separate follow-up (below it, the key is a compile error,
`V-002`).

```yaml
obligations:
  - { id: "I-1", kind: invariant, text: "No convergence step loses a refusal, a detector id, or an offset that 013's corpus records.", anchor: "3-1-what-moves-and-what-stays" }
  - { id: "I-2", kind: invariant, text: "aicortex keeps its own verdict, normalization and keyed-digest Decision; only detection is consumed.", anchor: "3-1-what-moves-and-what-stays" }
  - { id: "R-1", kind: requirement, text: "After this spec lands, aicortex-gate has one detector table, action-gate's, at an exact pin.", anchor: "3-2-consumption" }
```

## Verification

```verify:cli
# planned: runs once the upstream release exists and this spec is built.
cargo test -p aicortex-gate --locked
```
