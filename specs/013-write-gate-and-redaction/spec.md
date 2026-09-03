---
id: "013-write-gate-and-redaction"
title: "The write gate: refuse secrets, cap size, establish origin, quarantine the rest"
status: approved
kind: "kernel"
domain: "memory"
created: "2026-09-03"
authors: ["Bartek Kus"]
implementation: pending
risk: critical
wave: 1
depends_on:
  - "012-store-schema-and-repositories"
establishes:
  - "crates/aicortex-gate/Cargo.toml"
  - "crates/aicortex-gate/src/lib.rs"
  - "crates/aicortex-gate/src/rules.rs"
  - "crates/aicortex-gate/src/secrets.rs"
  - "crates/aicortex-gate/src/limits.rs"
  - "crates/aicortex-gate/src/verdict.rs"
  - "crates/aicortex-gate/tests/gate.rs"
  - "crates/aicortex-gate/testdata/corpus/"
extends:
  - { spec: "010-chassis-adoption-and-workspace", unit: { kind: section, file: "Cargo.toml", anchor: "workspace.dependencies" }, nature: additive }
constrains:
  - { flavor: invariant-freeze, unit: "crates/aicortex-store/src/memory_repo.rs", note: "no insert path exists that has not passed a Verdict" }
summary: >
  Everything that enters the store passes one gate, and the gate runs before
  the transaction opens. It refuses credentials rather than redacting them,
  because a redacted secret is still a secret that was transmitted, stored
  in a log, and possibly embedded. It caps size, requires an establishable
  origin, and quarantines rather than admits when origin is doubtful. Every
  refusal is a ledgered Decision with a reason code and never the offending
  content. The rule set is data, the corpus of fixtures is committed, and
  the gate is a pure function so its verdicts are reproducible in review.
---

# 013: The write gate

## 1. Purpose

A memory store is a place people paste things. Some of those things are API
keys, private keys, session cookies, and passwords, pasted by a human in a
hurry or by an agent summarizing a terminal buffer. Once such a value is
stored it is also embedded, indexed, backed up, and returned by recall, and
every one of those is a copy. The only safe moment is before the write.

The gate is also where origin is established. Constitution IX says a memory
without provenance is not stored; this spec is the code that enforces it and
decides what happens to content whose origin is merely asserted by the
caller rather than attested.

## 2. Territory

The `aicortex-gate` crate: the rule engine, the secret detectors, the
limits, and the verdict type. It has no I/O and no async; it is a pure
function from a candidate to a `Verdict`, which makes it exhaustively
testable against a committed corpus. It freezes on the store's insert path
the property that no write happens without a verdict.

## 3. Behavior

- **B-1 (position).** `Gate::evaluate(&Candidate) -> Verdict` runs before
  any transaction opens. `MemoryRepo::insert` takes an `Admitted` value
  that only `Verdict::Admit` can produce, so an insert without a verdict
  does not typecheck.
- **B-2 (verdicts).** `Verdict` is `Admit(Admitted)`,
  `Quarantine(Admitted, Reason)`, or `Refuse(Reason)`. `Reason` is a closed
  enum with a stable code and a human sentence: `SecretDetected`,
  `TooLarge`, `EmptyAfterNormalization`, `OriginUnestablished`,
  `UnsupportedMedia`, `RateExceeded`, `PolicyDenied`.
- **B-3 (secrets are refused, never redacted).** A detected credential is
  a refusal of the whole candidate. The response names the reason and the
  offset, and contains neither the value nor a masked form of it. Redaction
  is rejected because it still stores a record that a secret existed at a
  place, still transmitted the value to this process, and still tempts a
  later reader into reconstructing it.
- **B-4 (detectors).** Prefix detectors for the well-known token shapes,
  PEM block detectors for private keys, a URL detector for embedded
  credentials, a JWT detector for three base64url segments with a
  decodable header, and a Shannon-entropy detector over long unbroken
  tokens with a configurable threshold. Every detector is data-driven from
  `rules.rs` so adding one is a table row and a fixture, not a code path.
- **B-5 (false positives are an operator decision).** A refusal can be
  overridden per candidate by an operator-authenticated call carrying the
  reason code and a justification. The override is a ledgered Decision
  naming the subject who made it, and the memory is admitted with
  `trust: Assertion` and a marker in its provenance. There is no
  configuration flag that disables the gate globally.
- **B-6 (limits).** Text longer than the ceiling (default 64 KiB), a
  candidate with more than the configured media references, and a
  normalized-empty body are refused. The ceiling is configurable downward
  only.
- **B-7 (origin).** A candidate carries a `SourceRef` and the authenticated
  actor. When the source is one the deployment trusts (an authenticated
  client, a registered adapter), the verdict may be `Admit`. When the
  source is asserted but unverifiable (a pasted export, an unauthenticated
  webhook that passed a shared-secret check only), the verdict is
  `Quarantine`, which stores the memory with `Status::Quarantined`,
  invisible to retrieval until a human or a curator promotes it.
- **B-8 (normalization).** Before evaluation, content is normalized: Unicode
  NFC, trailing whitespace stripped, zero-width and bidirectional control
  characters removed. The last is a safety property, not tidiness: those
  characters are used to hide instructions inside text that will be shown
  to a model.
- **B-9 (ledger).** Every `Refuse` and every `Quarantine` appends a
  Decision through rahi's ledger with the reason code, the scope, the
  actor, and a content hash, never the content. The request path does not
  await the append.
- **B-10 (determinism).** The gate is pure and has no clock, no randomness,
  and no network. The same candidate and rule set always yield the same
  verdict, so a fixture corpus is a regression suite.

## 4. Functional requirements

- **FR-001.** Every fixture in `testdata/corpus/` yields its recorded
  verdict and reason code. The corpus contains at minimum: a plain note, a
  note containing each detector's positive case, a note with a
  high-entropy but benign identifier that must be admitted, an oversize
  note, a note with bidirectional control characters, and an
  origin-unverifiable note.
- **FR-002.** A refusal response contains no substring of the offending
  value, asserted by searching the serialized error for the fixture's
  secret.
- **FR-003.** An operator override admits exactly one candidate and emits
  exactly one Decision naming the operator subject.
- **FR-004.** A compile-fail test asserts `MemoryRepo::insert` cannot be
  called with a candidate that has not been through the gate.
- **FR-005.** The entropy detector's false-positive rate on a committed
  corpus of benign identifiers is zero at the shipped threshold.

## 5. Acceptance criteria

- **AC-1.** `cargo test -p aicortex-gate --locked` passes over the whole
  corpus.
- **AC-2.** A capture of a fixture containing an API key is refused end to
  end, the store holds no row, and the ledger holds one Decision.

## 6. Out of scope

Redaction for display, which does not exist here. Rate limiting, which is
the chassis's (`rahi://020`, `rahi://025`). Promotion out of quarantine
(023). What the client is told, beyond the reason code (020, 021).

## 7. Resolved decisions

- **D-1 (2026-09-03, this spec).** Refuse rather than redact. Rejected
  alternative: store with the secret replaced by a placeholder, which was
  attractive because it preserves the surrounding note. It loses because
  the secret has already been transmitted and logged by then, and because
  a stored placeholder invites a future feature to "recover" the original.

## Verification

```verify:cli
cargo test -p aicortex-gate --locked
```
