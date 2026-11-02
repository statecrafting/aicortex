# aicortex-gate

The write gate: a pure function from a candidate memory or claim to a verdict. It refuses content that carries a credential rather than redacting it, and it has no clock, store, or randomness of its own.

Part of [aicortex](https://github.com/statecrafting/aicortex), persistent,
governed memory for AI clients, built on the rahi chassis. The five library
crates (`aicortex-types`, `aicortex-claims`, `aicortex-gate`,
`aicortex-store`, `aicortex-embed`) release together at one version; see
`CHANGELOG.md` in the repository for what each release carries. A release is
a publication of library crates, not a deployment or a claim of production
readiness.

Licensed under Apache-2.0.
