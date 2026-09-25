# Orchestrator rules

- Execute phased work in order; stop at human checkpoints.
- Write output files where the spec says; do not invent locations.
- Keep the working tree green; never leave the coupling gate red.
- Recompute derived artifacts (`spec-spine compile`, `spec-spine index`)
  before opening a PR, and commit the regenerated shards with the change that
  made them stale. A shard left uncommitted dirties the tree for whoever comes
  next.
- One pull request, one spec: follow `AGENTS.md` "Working the backlog"; a
  session may land several specs in build order (spec 048).
