// Antigravity capture handler (spec 022 B-4, B-8). UNVERIFIED: this client's
// hook surface has not been checked against a release, so the handler takes
// the neutral shape on stdin and the matrix row says `verified = false`.
//
//   { "session": "...", "events": [ { "event": "message", "role": "user",
//     "text": "..." }, { "event": "tool_result", "role": "tool" }, ... ] }
//
// Wire it to whatever session-end or turn-end command the client offers.
import { parseJson, readStdin, runHook } from "../capture-core.mjs";

const input = parseJson(await readStdin()) ?? {};
const events = Array.isArray(input.events) ? input.events : [];
await runHook({ client: "antigravity", session: input.session ?? "unknown", events });
