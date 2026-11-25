# aicortex-index

The in-process similarity scan over stored vectors and the application-owned text index: a deterministic tokenizer, postings kept in the store, and BM25 scoring. Every query compares only against vectors of its own model.

Part of [aicortex](https://github.com/statecrafting/aicortex), persistent,
governed memory for AI clients, built on the rahi chassis. The six library
crates (`aicortex-types`, `aicortex-claims`, `aicortex-gate`,
`aicortex-store`, `aicortex-index`, `aicortex-embed`) release together at one
version; see `CHANGELOG.md` in the repository for what each release carries.
A release is a publication of library crates, not a deployment or a claim of
production readiness.

Licensed under Apache-2.0.
