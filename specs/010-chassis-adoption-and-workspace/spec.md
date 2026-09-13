---
id: "010-chassis-adoption-and-workspace"
title: "The cell: a Cargo workspace, pinned rahi crates, a declared manifest, and one line of main"
status: approved
kind: "kernel"
domain: "chassis"
created: "2026-09-03"
authors: ["Bartek Kus"]
implementation: in-progress
risk: critical
wave: 1
depends_on:
  - "002-memory-thesis"
establishes:
  - "Cargo.toml"
  - "rust-toolchain.toml"
  - "deny.toml"
  - "apps/aicortex/Cargo.toml"
  - "apps/aicortex/src/main.rs"
  - "apps/aicortex/src/cell.rs"
  - "apps/aicortex/manifest.toml"
  - "apps/aicortex/tests/cell.rs"
extends:
  - { spec: "001-agentic-harness", unit: "spec-spine.toml", nature: additive }
summary: >
  The workspace and the seam to the chassis. rahi crates are pinned to one
  exact version in the workspace manifest and are never vendored or patched.
  The application is a `Cell`: it returns a manifest, a migration list, a
  router, and an operator router, and `main` hands it to rahi's runner,
  which owns argument parsing, boot, the verbs, and the exit codes. The
  manifest is the capability ceiling, and it starts closed: no egress host
  is declared until a spec needs one.
---

# 010: The cell

## 1. Purpose

Everything this product does happens inside a chassis that already exists.
This spec is the seam: it fixes how rahi is depended on, what the
application owes it, and what the application is forbidden from
reimplementing. Constitution VI is frozen here in buildable form.

The failure this forecloses is the one that killed the predecessor's
architecture: each capability became a copy of the whole server, so a fix
in one never reached the others (`openbrain://fork-per-feature`). If the
application is a `Cell` and nothing else, there is nowhere for a fork to
start.

## 2. Territory

The workspace root manifests, the toolchain pin, the supply-chain policy,
and the binary crate under `apps/aicortex`. Later specs extend `cell.rs`
additively as they add routes and migrations; each such spec declares an
`extends` edge on it.

Operator prerequisite (amended 2026-09-12, D-2): the nine rahi crates of
B-2, `rahi-cli` included, published to a registry at one exact version with
a matching release tag. Preparing this spec's code may begin before that
release exists; its acceptance cannot pass, and the spec cannot flip to
`implementation: complete`, until it does.

## 3. Behavior

- **B-1 (workspace).** A virtual workspace at the root with
  `[workspace.dependencies]` as the single place any third-party or rahi
  version is written. Crate manifests inherit with `workspace = true`.
  Rust edition 2024, toolchain pinned in `rust-toolchain.toml`, `unsafe`
  forbidden workspace-wide, clippy lints denied at the workspace level
  (`unwrap_used`, `expect_used`, `indexing_slicing`, `panic`), with
  `float_arithmetic` allowed only in `aicortex-index` and
  `aicortex-recall` through one documented `allow` at each crate root.
- **B-2 (rahi is pinned, never forked).** `rahi-types`, `rahi-store`,
  `rahi-ledger`, `rahi-kernel`, `rahi-idp`, `rahi-edge`, `rahi-ops`,
  `rahi-harness`, and `rahi-cli` are declared with one exact `=x.y.z`
  version, the same for all nine (amended 2026-09-12, D-2: `rahi-cli` was
  missing although B-3's `main` calls it). No `[patch]` section, no `path`
  dependency, no `git` dependency, no vendored copy, no fork. A test asserts
  the manifest contains no `[patch]` table, no rahi dependency with a `path`
  or `git` key, and all nine rahi crates at one version.
- **B-3 (the Cell).** `struct Aicortex` implements rahi's `Cell`:
  `manifest()` returns the embedded `manifest.toml`; `migrations()` returns
  the ordered migration list assembled from every crate that owns schema;
  `routes(state)` returns the merged product router; `operator_routes(state)`
  returns the operator surface. `main` is `rahi_cli::run(Aicortex)` and
  contains no argument parsing, no logging setup, and no listener.
- **B-4 (manifest starts closed).** `manifest.toml` declares identity,
  services, resources, and a capability catalog. `egress` is an empty list
  in this spec: the first host is added by the spec that needs it, with
  the reason in that spec's Behavior. Deny by default is the chassis's
  (`rahi://015`), and the ceiling being empty at the start is what makes
  additions visible in review.
- **B-5 (state).** `AppState` holds the rahi `Store`, `Ledger`, `Kernel`,
  and the product's own handles. It is constructed once by `cell.rs` and
  cloned into handlers. No global, no lazy static holding a connection.
- **B-6 (exit codes and errors).** The product's error type maps onto
  rahi's `Error` variants and inherits its exit codes; no new top-level
  exit code is invented.
- **B-7 (supply chain).** `deny.toml` refuses unmaintained and yanked
  crates, restricts licences to a permissive allowlist compatible with
  Apache-2.0 distribution, and denies duplicate major versions of the
  cryptographic and HTTP stacks.
- **B-8 (no reimplementation).** A test greps the workspace and fails on
  an `axum::Router::new()` outside `cell.rs` and the surface crates that
  spec 020 and 021 own, on any direct `hiqlite::` use, and on any
  `reqwest::Client::new()` outside the governed egress call sites.

## 4. Functional requirements

- **FR-001.** `cargo build --workspace --locked` succeeds with only the
  binary and no library crates yet.
- **FR-002.** `aicortex --help` lists rahi's verbs (`serve`, `preflight`,
  `migrate`, `backup`, `restore`, `ledger verify`) without the application
  declaring any of them.
- **FR-003 (amended 2026-09-13, D-3).** `tests/cell.rs` parses the
  embedded manifest with the chassis's `Manifest::parse`, computes its
  hash, and finds `resources.egress` empty; it finds `Aicortex::migrations()`
  empty, which is schema version 0. `aicortex preflight` against an empty
  temporary data directory prints every chassis check by name, in the
  chassis's order, and fails exactly one of them, `keys`.
- **FR-004.** The B-2 and B-8 grep tests fail when a `[patch]` table or a
  stray router is introduced.
- **FR-005.** The B-2 test fails when any of the nine rahi crates is
  missing, carries a `git` or `path` key, or is pinned to a version that
  differs from the other eight.

## 5. Acceptance criteria

- **AC-1.** `cargo test -p aicortex --locked` passes.
- **AC-2.** `cargo deny check` passes.
- **AC-3 (amended 2026-09-13, D-3).** `aicortex preflight` on an empty
  temporary data directory exits 1 with `keys` as its only failing check,
  names every other check, and starts no network listener.

## 6. Out of scope

Every table (012), every route (020, 021), and every capability the
manifest will eventually declare. The reference deployment (044).

## 7. Resolved decisions

- **D-1 (2026-09-03, this spec).** rahi is consumed from a registry by
  exact version rather than as a path dependency in a shared workspace,
  even during co-development. A path dependency makes the chassis
  contract mutable from inside the product, which is precisely the
  coupling that constitution VI forbids. The cost is a publish step per
  chassis change; the benefit is that a chassis bug cannot be hidden by a
  local edit.
- **D-2 (2026-09-12, amendment, revision-4 AI-01 with rahi RH-05).** B-2
  named eight rahi crates while B-3 requires `rahi_cli::run`, so the
  workspace could not build as specified; `rahi-cli` joins the pinned set.
  At the time of this amendment rahi has nine crates at `0.1.0`, no tags and
  no publication. The maintainer kept D-1's published-crate policy rather
  than relaxing it: rahi's RH-05 chooses registry publication of all nine
  crates with matching release tags, and this spec waits for that release.
  A Git revision is useful for a local spike and does not satisfy D-1;
  there is no Git dependency waiver, stated or implied. A session may
  prepare the workspace before the release (AGENTS.md step 7 keeps the
  spec `in-progress` with a dated Status note naming the missing release),
  but no acceptance criterion is reported as passed against anything other
  than the published crates. The maintainer adopted this on 2026-09-12
  through revision 4's aicortex decisions; the agent authored the text and
  this entry records that authority rather than assuming it.
- **D-3 (2026-09-13, amendment of FR-003 and AC-3).** FR-003 and AC-3 were
  written before the chassis's preflight existed, and they contradict it.
  At rahi `444bcf8`, preflight reports nine named checks (`config`,
  `data_dir`, `keys`, `restore_env`, `hiqlite`, `engine`, `rauthy`,
  `ledger`, `disk`) as PASS, FAIL, or SKIP and exits 1 on any failure. On
  an empty data directory the key set is missing, so `keys` fails and the
  store, engine, rauthy, and ledger checks are skipped: exit 1, never 0.
  Its report prints no manifest hash, no egress list, and no schema
  version. The chassis's own binary showed exactly this against an empty
  scratch directory on 2026-09-12. Because `main` is one line (B-3), the
  application cannot add to that output. The maintainer chose to amend
  this spec to the chassis. The manifest hash, the empty egress list, and
  schema version 0 are asserted in `tests/cell.rs` from the embedded
  manifest and the migration list, and the preflight criterion becomes the
  report it actually gives on an empty directory. Rejected: asking rahi to
  print those values and pass on a fresh volume, which would add a chassis
  spec and release to a bootstrap already waiting on one; and dropping the
  preflight criteria, which would leave the verb untested here. A preflight
  that exits 0 needs `first-boot` and a rauthy on loopback, which 002 §5
  places in wave 2, so it is not this spec's acceptance. The maintainer
  decided this on 2026-09-13; the agent authored the text and this entry
  records that authority rather than assuming it.
- **D-4 (2026-09-13, build: where the cell and its state live).** The
  chassis's `Cell` trait (rahi `444bcf8`, `rahi-cli`) has associated
  functions and no receiver, and it hands every router the chassis's own
  `rahi_edge::AppState`, which the chassis builds at boot around the store,
  the ledger, and the kernel. So `Aicortex` is a unit struct, and in this
  spec `cell.rs` builds no state: there are no product handles yet, and the
  one state value is cloned into handlers by the chassis, with no global
  and no lazy static holding a connection. B-5's sentence that `cell.rs`
  constructs `AppState` describes a chassis API that was not built that
  way; its invariant holds, and a later spec that adds product handles
  decides how they reach handlers. `cell.rs` is also the package's library
  root, with `main.rs` as the one-line binary, so `tests/cell.rs` can hold
  `Aicortex` to its declarations without a file outside this spec's
  territory. Rejected: a separate `lib.rs`, which adds a file to
  establish for no behaviour, and `#[path]` includes from the test, which
  compile the cell twice. B-6 has nothing to map yet: no product error
  type exists, and the first spec with fallible product code defines one.
- **D-5 (2026-09-13, build: the first source files meet the hash lint).**
  With the workspace present, `spec-spine lint --fail-on-warn` refuses the
  whole-file claims on files no content hash covers (`L-008`), the corpus
  being silent on it. The two pins, `rust-toolchain.toml` and `deny.toml`,
  join `[index] extra_hashed_inputs`, so a pin change restamps every shard
  as a governance change should. Rust sources and app manifests go to
  `[lint] unwitnessed_allowed`, the spec-spine 057 instrument rahi uses for
  the same case: hashing source would stale every shard on every edit, the
  coupling gate and `require_ownership` still refuse an unowned or uncoupled
  edit, and `check` keeps reporting the count. `spec-spine.toml` is 001's,
  hence this spec's `extends` edge on it.
- **D-6 (2026-09-13, build: what B-7 can refuse on the chassis's tree).**
  `cargo deny check` against the chassis's dependency tree found three
  things this workspace cannot fix without patching rahi, which B-2 and
  constitution VI forbid. The Mozilla root store (`webpki-root-certs`,
  under hiqlite's TLS stack) is CDLA-Permissive-2.0, a permissive data
  licence, so it joins the allowlist. `bincode` 2 (RUSTSEC-2025-0141,
  unmaintained, under hiqlite and cryptr) and `quick-xml` 0.39
  (RUSTSEC-2026-0194 and 0195, under s3-simple) are ignored by id with the
  path each arrives by. `unmaintained` stays `all`, so any new unmaintained
  crate is still refused. Duplicate versions are refused for the entry
  crates of both stacks (`ring`, `rustls`, `aws-lc-rs`, `ed25519-dalek`,
  `http`, `hyper`, `h2`, `axum`, `tower`, `tower-http`, `reqwest`), none of
  which is duplicated today. The chassis already carries two copies of
  `aws-lc-sys` and of the primitives `digest`, `curve25519-dalek`,
  `chacha20`, `block-buffer`, and `crypto-common`, so those stay warnings
  and are recorded in `deny.toml` as rahi's to converge. MPL-2.0 is not
  allowed, because nothing in the tree needs it. Every ignore and gap is
  revisited at the version bump that names the published release.

## Status (2026-09-13, in progress: waiting on the published rahi release)

Prepared: the eight files this spec establishes, and D-4 to D-6.

- In this repository `cargo build --workspace --locked` exits 101 with "no
  matching package named `rahi-cli` found" in the crates.io index. No
  `Cargo.lock` is committed, because none can be resolved against a
  registry that does not carry the crates. So `make ci`'s cargo steps and
  this spec's Verification block fail until the release; the governance
  half of the gate passes.
- Spike evidence, which D-2 says is not acceptance: a scratch copy of these
  files, with an uncommitted `[patch.crates-io]` in a parent
  `.cargo/config.toml` pointing at a `git archive` of rahi `main` at
  `52ad1a7`, passes `cargo build --locked`, `cargo test --locked` (7 of 7),
  `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`, and
  `cargo deny check`. Negative controls on real files each fail the test
  that owns them: a `[patch]` table in `Cargo.toml` fails B-2, a stray
  router and a direct hiqlite use fail B-8, and a declared egress host
  fails B-4.
- Remaining: rahi's RH-05 release of the nine crates with a matching tag.
  Then one change bumps the nine pins to that version, commits the
  `Cargo.lock` it resolves, revisits D-6, and runs `make ci` and
  `spec-spine verify 010-chassis-adoption-and-workspace`. Only then does
  this spec flip to `complete`.

## Verification

```verify:cli
cargo test -p aicortex --locked
cargo deny check
```
