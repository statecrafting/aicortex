# Review guide: aicortex

Read after `.statecraft/REVIEW.md`. This states what is particular to
aicortex: which checks the gate already enforces over the whole tree, so
the review need not infer them from hunks, and which surfaces carry the
most risk.

## Enforced by the gate, not visible in the diff

- **Derived shards.** Everything under `.statecraft/derived/` is excluded
  from the review diff by the profile. Its freshness against every spec and
  hashed input is enforced by `spec-spine check --fail-on-warn` in the
  required `spec-spine gate` job, and the per-commit step reruns it on every
  commit. A missing or stale shard fails that job; it is not shown by the
  absence of derived paths in the hunks.
- **Ownership claims.** Every file inside a crate must be claimed by a spec
  (`require_ownership` is on), and every changed path needs an authoring
  edit to an owning spec (`spec-spine couple`). Both run over the whole tree
  in the required gate. A claim usually sits in front matter the change does
  not touch, so it is not in the hunks. Report an unclaimed file only when
  the supplied front matter shows the claim is absent.
- **An `extends` edge that would form a cycle.** When the owner of a unit
  already extends the spec changing it, the change records a dated decision
  entry in the owning spec instead of a second edge; the coupling gate
  accepts that authoring edit.
- **Policy isolation.** A change to a `Cargo.toml`, `Cargo.lock`,
  `deny.toml` or `spec-spine.toml` lands in its own pull request, ahead of
  the code that needs it (spec 044 section 3.3). A spec in a later pull
  request may therefore cite, or declare an edge on, a configuration or
  manifest line that is already on the base and not in this diff.

## Surfaces that carry the most risk

1. **Scope.** Every read is scoped in its SQL by `scope_id` resolved from
   the caller's subject; a query that can return another scope's row is a
   blocking defect.
2. **Recalled content is data.** Every path that returns memory content to
   a model frames it through spec 019's envelopes; a raw body on such a path
   is a blocking defect.
3. **Forgetting.** Erasure reaches content, chunks, embeddings, index
   entries, mentions and edges.
4. **Identity and egress.** No credential is minted here and no static
   shared key is accepted; every outbound call goes through the kernel's
   governed facade.
