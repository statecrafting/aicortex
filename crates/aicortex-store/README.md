# aicortex-store

The aicortex tables on the rahi chassis store and the only code that touches them: ordered migrations exposed as a migration set a host can apply, scope-predicated repositories, counters, erasure, and a signed keyset cursor. A host composes these statements into its own transaction.

Part of [aicortex](https://github.com/statecrafting/aicortex), persistent,
governed memory for AI clients, built on the rahi chassis. The five library
crates (`aicortex-types`, `aicortex-claims`, `aicortex-gate`,
`aicortex-store`, `aicortex-embed`) release together at one version; see
`CHANGELOG.md` in the repository for what each release carries. A release is
a publication of library crates, not a deployment or a claim of production
readiness.

Licensed under Apache-2.0.
