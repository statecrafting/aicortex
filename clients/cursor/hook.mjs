// Cursor agent hooks: `beforeSubmitPrompt` and `afterAgentResponse` each run
// this program with one JSON object on stdin (spec 022 B-4, B-8).
//
//   beforeSubmitPrompt:  { conversation_id, hook_event_name, prompt, ... }
//   afterAgentResponse:  { conversation_id, hook_event_name, text, ... }
//
// Shell, MCP, and file events are tool events and are never read for content;
// register only the two above.
import { message, parseJson, readStdin, runHook } from "../capture-core.mjs";

const input = parseJson(await readStdin()) ?? {};
const events = [];
if (input.hook_event_name === "beforeSubmitPrompt" && typeof input.prompt === "string") {
  events.push(message("user", input.prompt));
} else if (input.hook_event_name === "afterAgentResponse" && typeof input.text === "string") {
  events.push(message("assistant", input.text));
} else if (input.hook_event_name) {
  events.push({ event: input.hook_event_name, role: "tool" });
}
await runHook({ client: "cursor", session: input.conversation_id ?? "unknown", events });
