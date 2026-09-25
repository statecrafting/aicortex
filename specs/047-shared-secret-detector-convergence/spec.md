---
id: "047-shared-secret-detector-convergence"
title: "Converge secret detection upward: aicortex's detectors move into action-gate, and aicortex consumes them"
status: approved
kind: "feature"
domain: "memory"
created: "2026-09-24"
authors: ["Bartek Kus"]
implementation: complete
risk: high
wave: 4
depends_on:
  - "013-write-gate-and-redaction"
establishes:
  - "crates/aicortex-gate/tests/registry.rs"
extends:
  - { spec: "013-write-gate-and-redaction", unit: "crates/aicortex-gate/src/rules.rs", nature: additive }
  - { spec: "013-write-gate-and-redaction", unit: "crates/aicortex-gate/src/secrets.rs", nature: additive }
  - { spec: "013-write-gate-and-redaction", unit: "crates/aicortex-gate/Cargo.toml", nature: additive }
  - { spec: "013-write-gate-and-redaction", unit: "crates/aicortex-gate/testdata/corpus/", nature: additive }
  - { spec: "013-write-gate-and-redaction", unit: "crates/aicortex-gate/tests/gate.rs", nature: additive }
  - { spec: "010-chassis-adoption-and-workspace", unit: { kind: section, file: "Cargo.toml", anchor: "workspace.dependencies" }, nature: additive }
references:
  - { unit: { kind: file, path: "specs/013-write-gate-and-redaction/spec.md" }, role: constraint }
obligations:
  - { id: "I-1", kind: invariant, text: "No convergence step loses a refusal, a detector id, or an offset that 013's corpus records.", anchor: "3-1-what-moves-and-what-stays" }
  - { id: "I-2", kind: invariant, text: "aicortex keeps its own verdict, normalization and keyed-digest Decision; only detection is consumed.", anchor: "3-1-what-moves-and-what-stays" }
  - { id: "R-1", kind: requirement, text: "After this spec lands, aicortex-gate has one detector table, action-gate's, at an exact pin.", anchor: "3-2-consumption" }
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

- **D-3 (2026-09-25, build session; the prerequisite and the pin).**
  action-gate-core 0.2.0 is on crates.io and carries B-1's detectors as a
  registry (`action_gate_core::secrets`: `PREFIX_RULES`, `PEM_RULES` with
  all seven markers, the URL, JWT, entropy and assignment detectors, and
  `scan` returning a detector id and a byte offset), ported from this
  crate's tables at `a927f85`, with golden vectors behind the
  `golden-vectors` feature. B-7 is therefore met. The workspace pins
  `action-gate-core = "=0.2.0"` with default features only; aicortex-gate's
  tests alone enable `golden-vectors`.
- **D-4 (2026-09-25, build session; no regex).** The registry path is
  regex-free (action-gate 001 D-1); `regex` sits behind action-gate's
  `checks-common` feature, which this workspace does not enable, so B-4's
  owner decision is not needed and `regex` does not enter the tree.
- **D-5 (2026-09-25, build session; the id mapping).** aicortex keeps its
  own `DetectorId`, which `Reason::SecretDetected` and every stored
  Decision name. `DetectorId::from_registry` maps a registry id to it, and
  the mapping is the identity on names: the registry kept every id this
  crate shipped (action-gate 001 B-1: an id is never renamed or reused).
  `tests/registry.rs` asserts the identity for every id the registry
  reports and every id the corpus records, so an upstream rename fails
  this build rather than changing what a refusal says. `rules.rs` keeps the
  `DetectorId`, the mapping and the `RuleSet`; `SecretRules` and
  `EntropyRule` are re-exported from the registry; `secrets.rs` keeps only
  `Finding` and a `scan` that runs the registry on the normalized text.
  Rejected: an explicit table of 35 `(aicortex id, registry id)` pairs,
  which would restate identical strings and be a second table to drift.
- **D-6 (2026-09-25, build session; parity evidence).** B-3's parity is
  shown three ways in `tests/registry.rs`: every golden vector (67) holds
  through this crate's `scan` by id, detector and offset; every corpus
  fixture the registry ported is found under its name with the same
  detector and, where the gate scans exactly the vector's text, refuses
  through the whole gate at the vector's offset; and FR-003 is asserted over
  the crate's sources (no prefix or armour table, token scan, JWT, URL or
  entropy code) and over its public rule types, which are the registry's.
  013's corpus test runs unchanged apart from counting the assignment
  detector among those that need a positive fixture. The two B-6 fixtures
  are `secret-credential-assignment.json` and
  `admit-prose-mentions-token.json`.
- **D-7 (2026-09-25, build session; a second copy upstream of here).**
  `cargo deny check` passes, and warns that `action-gate-core` appears
  twice: `rahi-kernel 0.2.0` depends on 0.1.0. That copy is the chassis's
  and converges when rahi moves its own pin; `action-gate-core` is not in
  `deny.toml`'s refuse-duplicates list, and adding it would fail a check
  this workspace cannot fix (the same reasoning as 010 D-6).

## 8. Follow-ups and open questions

- **F-1 (upstream spec, action-gate).** A spec in action-gate that adopts
  B-1's detectors with stable ids and byte spans, keeps them pure and
  deterministic, and publishes a release aicortex can pin. Not authored
  here.
- **Q-1 (regex).** Resolved by D-4: the upstream detectors do not need it.

## 9. Obligations

Declared in the frontmatter `obligations` key (spec-spine 106 grammar),
lifted there from this section once the pin moved to 0.25.0 (050 D-8,
001 D-11).

## Verification

```verify:cli
cargo test -p aicortex-gate --locked
cargo test -p aicortex-gate --locked --test registry
```
