// Claude Code SessionEnd hook: reads the session transcript and sends the
// durable items in it to POST /api/v1/capture (spec 022 B-4, B-8).
//
// stdin is Claude Code's hook input: { session_id, transcript_path, ... }.
// The transcript is JSON lines; each line has a `type` and a `message` whose
// content is a string or a list of blocks. Only `text` blocks of user and
// assistant messages are messages; `tool_use` and `tool_result` blocks are
// tool events and are never read for content.
import fs from "node:fs";
import { message, parseJson, readStdin, runHook } from "../capture-core.mjs";

const input = parseJson(await readStdin()) ?? {};
const events = [];
try {
  for (const line of fs.readFileSync(input.transcript_path, "utf8").split("\n")) {
    const entry = parseJson(line);
    if (!entry || entry.isMeta || (entry.type !== "user" && entry.type !== "assistant")) continue;
    const content = entry.message?.content;
    const role = entry.message?.role ?? entry.type;
    if (typeof content === "string") events.push(message(role, content));
    else if (Array.isArray(content)) {
      for (const block of content) {
        if (block?.type === "text") events.push(message(role, block.text));
        else events.push({ event: block?.type ?? "unknown", role: "tool" });
      }
    }
  }
} catch {
  // An unreadable transcript sends nothing.
}
await runHook({ client: "claude-code", session: input.session_id ?? "unknown", events });
