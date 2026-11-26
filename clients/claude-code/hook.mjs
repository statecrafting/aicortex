// Claude Code SessionEnd hook: reads the session transcript and sends the
// durable items in it to POST /api/v1/capture (spec 022 B-4, B-8).
//
// stdin is Claude Code's hook input: { session_id, transcript_path, ... }.
// The transcript is JSON lines; each line has a `type` and a `message` whose
// content is a string or a list of blocks. Only `text` blocks of user and
// assistant messages are messages; `tool_use` and `tool_result` blocks are
// tool events and are never read for content.
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { message, parseJson, readStdin, record, runHook } from "../capture-core.mjs";

const input = parseJson(await readStdin()) ?? {};
// The transcript is read only from Claude Code's own projects directory
// (AICORTEX_TRANSCRIPT_ROOT overrides it), and only as a .jsonl file, so a
// hook input that names any other local file reads nothing.
function transcriptFile(named) {
  const root =
    process.env.AICORTEX_TRANSCRIPT_ROOT ??
    path.join(process.env.CLAUDE_CONFIG_DIR ?? path.join(os.homedir(), ".claude"), "projects");
  if (typeof named !== "string" || !named.endsWith(".jsonl")) return null;
  try {
    const real = fs.realpathSync(named);
    const base = fs.realpathSync(root);
    return real.startsWith(base + path.sep) && fs.statSync(real).isFile() ? real : null;
  } catch {
    return null;
  }
}
const transcript = transcriptFile(input.transcript_path);
if (input.transcript_path && !transcript) {
  record("claude-code: the transcript is not a .jsonl file under the projects directory; nothing read");
}
const events = [];
try {
  for (const line of (transcript ? fs.readFileSync(transcript, "utf8") : "").split("\n")) {
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
