# Rahi chassis prerequisites for aicortex

**Refreshed 2026-10-10 by spec 058** (rahi 0.6.0 adoption). The first
version of this note, written 2026-09-19 for spec 014, named three chassis
prerequisites and the rahi spec that owned each. All three were met on rahi
0.4.0, and that history now lives in spec 014's dated Status entries. This
note records what aicortex consumes from the chassis today and what it still
waits on. It is read-only with respect to rahi: chassis work is drafted in
rahi and released by rahi's own process.

## 1. The 2026-09-19 handoff, closed

| Item | Resolution | Evidence |
|---|---|---|
| H-1. A published release carrying lifetime ledger identity (rahi 042) | Met on rahi 0.2.0, adopted by spec 046. | spec 014 Status (2026-09-25): the archived-retry diagnostics became positive assertions (014 D-13). |
| H-2. A registry-resolvable store with the stale-lease-release repair | Met on rahi 0.4.0, which depends on the published `hiqlite-patched` 0.15.0-patched.3 with no `[patch]`, path or git source. | spec 014 Status (2026-09-26): stale release after TTL takeover, then unrelated and same-key lock progress, proven. |
| H-3. Full-node restart followed by a second lease | Met on rahi 0.4.0. | spec 014 Status (2026-09-26): a node stopped with its lease held reopens and completes a fresh erasure exactly once after `LEASE_TTL_SECONDS + 1`. |

Spec 014 is `implementation: complete`.

## 2. What aicortex consumes as of 2026-10-10

All nine rahi crates at exact version `0.6.0` and action-gate-core at
`0.3.0` (spec 058). The parts of rahi 0.5.0 and 0.6.0 that matter here:

| Chassis capability | rahi spec | aicortex consumer | State |
|---|---|---|---|
| Managed services joined through `serve`'s lifecycle (`Cell::services`) | 047 | spec 015 B-2: the embedding worker (`run_worker`) | Available; mounting it is 015's remaining work. |
| Cell-contributed preflight checks (`Cell::preflight_checks`) | 049 | spec 015 B-3, AC-2, FR-003, FR-005: `EmbeddingConfig::check` and the embedding preflight report | Available; contributing the check is 015's remaining work. |
| Binding document and `Cell::app_revision` | 040 | none yet (packaging, spec 043) | Available; not overridden. |
| Readiness window on stop (`/readyz` 503 `stopping`) | 043 B-9 | the deployment probes (spec 043) | Inherited without code change. |
| Signal exits through shutdown | 048 | `serve` and the verbs | Inherited without code change. |

The cell overrides none of the new `Cell` methods as of spec 058, so its
lifecycle is the 0.4.0 one until spec 015 mounts the worker and the check.

## 3. Known chassis-side duplicates

`deny.toml` (spec 010 D-6) refuses two copies of any crate in the HTTP and
cryptographic stacks. rahi 0.6.0 brings two copies this workspace cannot
converge, recorded in spec 058 D-3:

- `tower-http` 0.6.11 (through `reqwest` under hiqlite) beside 0.7.1
  (through `rahi-edge`). Skipped by exact version.
- `action-gate-core` 0.2.0 (`rahi-kernel` pins `=0.2.0`) beside 0.3.0
  (aicortex-gate). Outside the refused stacks, reported as a warning; no
  action-gate type crosses between them.

Both lapse when a rahi release converges them. Neither needs a rahi
proposal from here: they are ordinary dependency moves rahi's Dependabot
grouping will surface.

## 4. Open questions for the chassis

- **Queue-health metrics (spec 015 B-3, D-18).** rahi's metric registry
  hook (`rahi_edge::obs::current`) is available. Embedding queue health is
  scope-bound (015 D-18), so a process-wide figure needs a separately
  authorized surface. Whether that surface is aicortex's or the chassis's is
  for spec 015 to decide; no rahi proposal is drafted.

No approved aicortex spec is blocked on an unreleased chassis capability
today.
