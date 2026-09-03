---
paths:
  - "crates/**"
  - "apps/**"
  - "eval/**"
  - "docker/**"
  - "deploy/**"
---

# Build commands and ownership

The `Makefile` is the source of truth for what CI validates; `make ci`
green locally means a green CI run. These are the commands behind it.

```sh
make spine                                                   # the spec-spine gate chain
make ci                                                      # spine + coverage + every cargo gate
cargo build --workspace --locked
cargo test --workspace --locked
cargo test -p aicortex-recall --locked --test fusion         # one crate, one test file
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo fmt --all --check                                      # fix with: cargo fmt --all
cargo deny check                                             # supply chain (deny.toml, spec 010)
scripts/verify-spec.sh <spec-id>                             # the spec's verify:cli blocks
```

Rules:

- Always `--locked`; `Cargo.lock` is committed and part of the determinism
  contract. Third-party versions live only in the root `Cargo.toml`
  `[workspace.dependencies]`; crate manifests inherit with
  `workspace = true`. Adding a dependency needs an `extends` edge on spec
  010's `Cargo.toml` section `workspace.dependencies`.
- The rahi crates are pinned to one exact version in
  `[workspace.dependencies]`, bumped in a single change that names the rahi
  release. Never vendor, patch, or fork them. If a chassis contract is
  wrong, stop and report: the fix is a spec in rahi.
- `unsafe` is forbidden workspace-wide; clippy runs with `-D warnings`;
  library code denies `unwrap_used`, `expect_used`, `indexing_slicing`, and
  `float_arithmetic` outside the ranking module, which declares its
  allowance in one place with a comment.
- Every crate manifest carries `[package.metadata.spec-spine] spec =
  "<founding spec id>"`.
- Claim every new file in the spec you are implementing (its
  `establishes` list) in the same change: `require_ownership` is on and
  `C-002` refuses an unclaimed source file at PR time. Touching a file
  another spec owns needs an `extends` edge on that spec's unit.
- Crates depend downward only: `aicortex-types`, then `aicortex-store` and
  `aicortex-gate`, then `aicortex-embed`, `aicortex-index` and
  `aicortex-graph`, then `aicortex-recall`, then `aicortex-api` and
  `aicortex-mcp`, then `aicortex-ingest`, `aicortex-curate` and
  `aicortex-eval`. `apps/aicortex` composes them; nothing depends on it.
- No `tsconfig.json` and no `package.json` at the repository root (spec
  001 D-1): the orchestrator gates a Rust target through `make ci`.
