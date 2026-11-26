# Antigravity

Everything here is generated from `clients/matrix.toml` and the deployment's
public URL: `GET /api/v1/clients/antigravity` returns the snippet with your URL
filled in, so copy that rather than editing a placeholder. The matrix row
for this client says whether it is verified; read it before relying on it.

## Connect (one URL)

Add the server entry from the served snippet to the client's MCP configuration and complete the browser sign-in. This row is **unverified** (see the matrix).

No key is pasted anywhere, because none exists.

## Install the capture hook

1. Copy this repository's `clients/` directory to `~/.aicortex/clients/`
   (the hook imports `../capture-core.mjs`). Node 20 or newer is required.
2. Antigravity has no hook surface verified here. The handler reads `{ "session": ..., "events": [...] }` on stdin; wire it to whatever turn-end command the client offers. Until then there is no automatic capture for this client.

## What automatic capture sends

Only durable items, never runtime state (spec 022 B-8). The hook sends a
line that a user or assistant message writes under one of five labels, and
each label names one memory kind:

| Label | Kind |
|---|---|
| `Preference:` | `Preference` |
| `Correction:` | `Correction` |
| `Decision:` | `Decision` |
| `Commitment:` | `Task` (a commitment that outlives the session, never progress) |
| `Remember:` | `Fact` (something you asked to keep) |

It never sends runtime state: work in progress, tool output, local file
system paths, connection or server settings, credentials, or which plugins,
servers, hooks, or skills are active. The hook decides by the structure the
client exposes (event type, tool name, message role) and never asks a model;
the write gate stays the backstop for credentials. A failed request is
silent to you and recorded in `~/.aicortex/hook.log`.

## Turn automatic capture off

Set `AICORTEX_CAPTURE=off` in the environment the client runs in, or remove
the hook entry below. Connecting the server does not depend on either.

## Credentials for the hook

The hook is a separate process from the client, so it cannot borrow the
client's session. It reads a bearer token for this client from
`AICORTEX_ACCESS_TOKEN` or from `~/.aicortex/antigravity.token` (override with
`AICORTEX_TOKEN_FILE`) and sends nothing when there is none. The token
should carry `memory.read` and `memory.write` only; `memory.promote` is never
requested (B-6). How a person obtains that token without pasting a key is not
yet specified (see spec 022 D-3).

Fixtures for this client are in `fixtures/`; the test
`cargo test -p aicortex-api --locked --test clients` drives them.
