# aicortex-claims

Typed claims checked against a domain's predicate registry, and the bitemporal projection over a claim history. Pure validation: no I/O, no SQL, no async runtime.

Part of [aicortex](https://github.com/statecrafting/aicortex), persistent,
governed memory for AI clients, built on the rahi chassis. The five library
crates (`aicortex-types`, `aicortex-claims`, `aicortex-gate`,
`aicortex-store`, `aicortex-embed`) release together at one version; see
`CHANGELOG.md` in the repository for what each release carries. A release is
a publication of library crates, not a deployment or a claim of production
readiness.

Licensed under Apache-2.0.
