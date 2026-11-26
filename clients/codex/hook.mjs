// Codex `notify` handler: Codex runs the program with one JSON argument after
// each turn (spec 022 B-4, B-8).
//
//   { "type": "agent-turn-complete", "thread-id": "...", "turn-id": "...",
//     "input-messages": ["..."], "last-assistant-message": "..." }
//
// The notification carries the user's messages and the assistant's last one
// and nothing else, so there is no tool output to exclude; any other
// notification type sends nothing.
import { message, parseJson, runHook } from "../capture-core.mjs";

const note = parseJson(process.argv[process.argv.length - 1]) ?? {};
const events = [];
if (note.type === "agent-turn-complete") {
  for (const text of note["input-messages"] ?? []) events.push(message("user", text));
  if (typeof note["last-assistant-message"] === "string") {
    events.push(message("assistant", note["last-assistant-message"]));
  }
}
await runHook({ client: "codex", session: note["thread-id"] ?? "unknown", events });
