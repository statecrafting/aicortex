# aicortex constitution

The principles that govern this repository. Principles I through V are the
governance substrate, inherited from the spec-spine generation this corpus
was bootstrapped in. Principles VI through XV are the product's own, and the
ones marked **frozen** are tier 1: they may be changed only by an amendment
that says what changed, why, and which specs are invalidated. A spec that
contradicts a frozen principle is wrong, and the coherence guard forbids
resolving that by editing the spec after the fact.

Several principles exist because the system this product replaces did the
opposite and the consequence was observable. Those carry an
`openbrain://` citation to the behavior, not to its source.

## I. Markdown-only authored truth

Humans and agents author markdown with YAML frontmatter. No authored JSON,
no authored YAML documents, no generated prose committed as if authored.

## II. Compiler-owned JSON machine truth

Machine-consumable truth about the corpus is emitted only by `spec-spine`
into `.derived/`. Both shard trees are committed; neither is hand-edited.

## III. Spec-first development

The corpus is the design. Every ordinary spec is a work order: `approved`
plus `implementation: pending`. Code arrives one spec per session, inside
that spec's declared territory, and every source file is claimed.

## IV. Determinism and validation

Compilation is a pure function of configuration and file contents. Staleness
is detected by content hash. The gate is mechanical and the same locally as
in CI.

## V. Approval is a human act

A machine-authored spec is born `draft`. Only a human moves a spec to
`approved`, and only an `approved` spec is ever scheduled.

## VI. The chassis is consumed, never forked (frozen)

Identity, state, the decision chain, the kernel, the edge, packaging, and
the operational verbs are rahi's. This repository pins rahi crates by
version and implements rahi's `Cell` trait: a manifest, migrations, routes,
operator routes, and a one-line main. When the chassis is wrong, the fix is
a spec and a release in rahi, never a local divergence, and never a second
HTTP server, store, or identity path inside this repository.

## VII. The subject is the only identity (frozen)

Every memory belongs to a scope owned by a rauthy `sub`. This repository
mints no credential, keeps no account row, and accepts no static shared key
in a header, a query string, or a configuration file
(`openbrain://static-shared-key`). Non-browser clients present
rauthy-issued, audience-bound bearer tokens validated by `rahi://025`.

## VIII. One store, one engine

Memories, embeddings, entities, edges, and provenance live in the
application's hiqlite SQLite group through rahi's store API. There is no
second database, no external vector service, and no loadable SQLite
extension (`rahi://016`). Similarity is computed in this process over
binary values this process wrote.

## IX. A memory is never a fact without provenance (frozen)

Every memory records where it came from, who or what asserted it, when, and
under which trust class. A memory with no provenance is not stored. An
inference derived from other memories names its sources. Content whose
origin cannot be established is quarantined, not admitted
(`openbrain://json-bag-schema`).

## X. Recalled content is data, never instruction (frozen)

Everything this system returns to a model is untrusted input. It is
delivered in a delimited block, labelled with its trust class and origin,
and framed so that instructions inside it are not to be followed. No
retrieval path may emit memory content in a position where a client would
reasonably treat it as a system directive. Promotion of a memory to
instruction grade, where a client is told to act on it, requires a human
decision recorded in the ledger.

## XI. Capture is transactional (frozen)

A capture writes the memory row and its work in one transaction, or writes
nothing. Embedding, extraction, and enrichment happen afterwards, driven by
the outbox, with retries and a dead letter state. A memory that exists but
cannot be found, because a later step failed and nothing retried it, is the
defining defect this principle forbids (`openbrain://orphaned-embedding`).

## XII. Every stored vector names its model

An embedding is stored with the model identifier, the dimension, and the
normalization it was produced under. A query embedding is compared only
against vectors from the same model. Changing models is a migration with a
re-embedding pass, never a silent reinterpretation of old bytes
(`openbrain://unversioned-embeddings`).

## XIII. Forgetting is a first-class operation

A memory can be superseded, corrected, expired, or erased. Erasure removes
the content, its embeddings, its derived chunks, and its index entries.
Memory content never enters the decision chain; the chain records that an
erasure happened and under whose authority, so an append-only ledger and a
right to be forgotten can both be true.

## XIV. Retrieval quality is measured or it is unknown

A change to chunking, embedding, ranking, or fusion is accompanied by a run
of the evaluation corpus, and the result is part of the gate. A system that
cannot say whether recall got worse will get worse.

## XV. The egress ceiling is declared

Every outbound call, including embedding and extraction, passes through the
kernel's governed egress facade and is within the declared manifest ceiling.
A deployment configured for local inference makes no outbound call at all,
and that is provable from the manifest rather than promised in a README.

## Amendment

Changing a frozen principle requires an amendment spec that states the
principle, the change, the reasoning, and the specs invalidated, and a human
approval on the record. Everything else changes by ordinary spec.
