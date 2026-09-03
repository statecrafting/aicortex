---
id: "010-chassis-adoption-and-workspace"
title: "The cell: a Cargo workspace, pinned rahi crates, a declared manifest, and one line of main"
status: approved
kind: "kernel"
domain: "chassis"
created: "2026-09-03"
authors: ["Bartek Kus"]
implementation: pending
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
  `rahi-ledger`, `rahi-kernel`, `rahi-idp`, `rahi-edge`, `rahi-ops`, and
  `rahi-harness` are declared with one exact `=x.y.z` version. No `[patch]`
  section, no `path` dependency, no vendored copy, no fork. A test asserts
  the manifest contains no `[patch]` table and no rahi dependency with a
  `path` or `git` key.
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
- **FR-003.** `aicortex preflight` against a temporary data directory
  reports the manifest hash, the empty egress list, and schema version 0.
- **FR-004.** The B-2 and B-8 grep tests fail when a `[patch]` table or a
  stray router is introduced.

## 5. Acceptance criteria

- **AC-1.** `cargo test -p aicortex --locked` passes.
- **AC-2.** `cargo deny check` passes.
- **AC-3.** `aicortex preflight` exits 0 on an empty data directory and
  prints the manifest hash.

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

## Verification

```verify:cli
cargo test -p aicortex --locked
cargo deny check
```
