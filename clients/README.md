# clients

Per-client adapters (spec 022). The server is client-agnostic; this tree is
the only place a client's name appears, and nothing here is imported by the
core.

- `matrix.toml`: the capability matrix, one row per client, each marked
  verified (with version, date, fixtures) or `verified = false`.
- `<client>/`: the configuration snippet (`snippet.txt`, with `{{mcp_url}}`
  and `{{public_url}}` filled in by `GET /api/v1/clients/{name}`), the
  capture hook (`hook.mjs`), a README with the exact steps, and
  `fixtures/` the test drives.
- `capture-core.mjs`: what every hook shares: the structural filter, the
  durable labels, the request, and the promise that a failure never reaches
  the client.

Connecting is one URL and a browser sign-in. Automatic capture sends durable
items only (preferences, corrections, decisions, commitments, facts you ask
to keep), never runtime state, and is turned off with `AICORTEX_CAPTURE=off`.
To add a client: a matrix row, a directory, a fixture set; the test refuses
a row with no directory or a directory with no row.

Entity pointers (spec 026) tell an agent that memory about a person or
project it just named already exists. They are returned inside a `search`
result when the request sets `pointers`, framed as untrusted data like every
recalled value, and no client hook carries them. `matrix.toml` records, per
client, its per-turn hook mechanism (`read_pointer`) and whether the client
places that hook's output in a data position (`data_position`). Every client
starts at `false`, because a hook's output usually reaches the model where it
is read as instruction; a row becomes `true` only when a test in
`crates/aicortex-api/tests/clients.rs` shows the client labels the text a hook
injects as untrusted, and it then names that test. A vendor statement is not
enough.
