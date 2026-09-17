---
id: "013-write-gate-and-redaction"
title: "The write gate: refuse secrets, cap size, establish origin, quarantine the rest"
status: approved
kind: "kernel"
domain: "memory"
created: "2026-09-03"
authors: ["Bartek Kus"]
implementation: in-progress
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
  - { flavor: invariant-freeze, unit: "crates/aicortex-store/src/erasure.rs", note: "erasure destroys the B-9 digest key of every Decision about the erased memory or scope" }
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

B-9's digest keys are not the gate's: they are minted where the Decision is
appended and held in the application store under 012's schema, so the gate
stays pure. This spec also freezes on 014's erasure path the property that
erasure destroys those keys (amended 2026-09-12, D-2).

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
  actor, and a keyed digest of the normalized content, never the content
  and never an unkeyed hash of it (amended 2026-09-12, D-2). The digest is
  HMAC-SHA-256 under a key minted for that one Decision. The Decision
  carries the digest, the algorithm identifier, and the key id; the key
  lives only in an application-store row naming the scope and, for a
  quarantine, the memory id. There is no shared or permanent digest key.
  Erasure destroys a key together with the object it covers: erasing a
  quarantined memory (014 B-7) destroys its Decision's key, and erasing a
  scope (014 B-9) destroys every key in the scope. A destroyed key leaves
  the chained digest as an opaque value that cannot be confirmed against a
  guessed body, which is what lets an append-only chain and constitution
  XIII both hold. The request path does not await the append.
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
- **FR-006.** A refusal's Decision carries the keyed digest, the algorithm
  identifier, and the key id. Neither the SHA-256 of the fixture body nor
  any substring of it appears in the serialized Decision; recomputing the
  keyed digest with the stored key reproduces it.
- **FR-007.** Two refusals of the same candidate mint two keys and two
  different digests, so a replayed candidate never recovers an earlier
  Decision's key. After a key row is destroyed, no store operation
  recomputes or returns that Decision's digest input.
- **FR-008.** No surface or document claims that erasure reaches refusal
  and quarantine records until 014's erasure tests show the key destroyed,
  a backup taken after the erasure holding no key, and no replay (capture,
  outbox, or import) restoring it. A backup taken before the erasure still
  holds the key until that backup is discarded; the erasure result says so
  rather than implying otherwise.

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
- **D-2 (2026-09-12, amendment, revision-4 AI-03).** B-9 appended an
  unkeyed content hash for every refusal and quarantine. For a short or
  predictable body that hash can be confirmed by guessing, and because the
  chain is append-only it can never be erased, which contradicts
  constitution XIII's "memory content never enters the decision chain" in
  effect if not in letter. Draft 045 surfaced the conflict when its own
  erasure rule refused to keep digests of erased bodies. The maintainer
  chose keyed digests with an erasure-scoped key lifecycle in the
  application store, for memories and coordination alike. Rejected: one
  deployment-wide digest key, which would make every digest confirmable
  for as long as the deployment exists and so defeat per-object erasure;
  and keeping the unkeyed hash while narrowing 045's rule, which leaves the
  guessing problem in place for memories. HMAC-SHA-256 and a key per
  Decision are this amendment's concrete choice; a later decision may
  change the construction only by recording the new algorithm identifier
  beside new digests and never by reinterpreting old ones. Erasure is not
  claimed for these records until key destruction, backups, and replay are
  tested (FR-008). The maintainer adopted this on 2026-09-12; the agent
  authored the text and this entry records that authority rather than
  assuming it.

## Verification

```verify:cli
cargo test -p aicortex-gate --locked
```
