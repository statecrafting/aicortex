// The one place a capture hook's rules live (spec 022 B-4, B-8).
//
// Each client's hook.mjs turns that client's own payload into a list of
// session events and calls `runHook`. Everything below that line is shared:
// which events may produce a memory, how a durable item is recognised, the
// request that is sent, and the promise that a failure never reaches the
// client (B-4, FR-004).
//
//   AICORTEX_URL           the deployment's public URL (the served snippet fills it in)
//   AICORTEX_ACCESS_TOKEN  a bearer token for this client, or
//   AICORTEX_TOKEN_FILE    a file holding one (default ~/.aicortex/<client>.token)
//   AICORTEX_CAPTURE=off   turns automatic capture off
//   AICORTEX_HOOK_LOG      where failures are recorded (default ~/.aicortex/hook.log)
//
// A session event is { event, role, text }. Only event "message" with role
// "user" or "assistant" can produce a memory. Tool calls, tool output, plugin
// and server activity, and system messages are never read for content. That
// is the structural filter of B-8; no model is asked anything.
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import crypto from "node:crypto";

// The labels a durable item is written under, and the kind each names (011 B-4).
export const LABELS = {
  preference: "preference",
  correction: "correction",
  decision: "decision",
  commitment: "task",
  remember: "fact",
};

const LINE = /^\s*(?:[-*]\s+)?(preference|correction|decision|commitment|remember)\s*:\s*(\S.*?)\s*$/i;
const MAX_TEXT = 2000;

export function extract(events) {
  const found = [];
  const seen = new Set();
  for (const item of events) {
    if (item?.event !== "message" || (item.role !== "user" && item.role !== "assistant")) continue;
    if (typeof item.text !== "string") continue;
    for (const line of item.text.split(/\r?\n/)) {
      const match = LINE.exec(line);
      if (!match) continue;
      const kind = LABELS[match[1].toLowerCase()];
      const text = match[2].slice(0, MAX_TEXT);
      const key = `${kind}\u0000${text}`;
      if (seen.has(key)) continue;
      seen.add(key);
      found.push({ kind, text });
    }
  }
  return found;
}

export function buildRequest(client, session, items) {
  return {
    candidates: items.map(({ kind, text }) => ({
      kind,
      text,
      source: { system: client, external_id: session },
    })),
  };
}

function home(...parts) {
  return path.join(process.env.AICORTEX_HOME ?? path.join(os.homedir(), ".aicortex"), ...parts);
}

function record(message) {
  try {
    const file = process.env.AICORTEX_HOOK_LOG ?? home("hook.log");
    fs.mkdirSync(path.dirname(file), { recursive: true });
    fs.appendFileSync(file, `${new Date().toISOString()} aicortex-capture ${message}\n`);
  } catch {
    // The log is best effort; a hook that cannot log still must not fail.
  }
}

function token(client) {
  if (process.env.AICORTEX_ACCESS_TOKEN) return process.env.AICORTEX_ACCESS_TOKEN.trim();
  const file = process.env.AICORTEX_TOKEN_FILE ?? home(`${client}.token`);
  try {
    return fs.readFileSync(file, "utf8").trim();
  } catch {
    return "";
  }
}

export async function runHook({ client, session, events }) {
  try {
    if ((process.env.AICORTEX_CAPTURE ?? "").toLowerCase() === "off") return;
    const items = extract(events);
    if (items.length === 0) return;
    const base = (process.env.AICORTEX_URL ?? "").replace(/\/+$/, "");
    if (!base) return record(`${client}: AICORTEX_URL is not set; nothing sent`);
    const bearer = token(client);
    if (!bearer) return record(`${client}: no access token; nothing sent`);
    const body = JSON.stringify(buildRequest(client, session, items));
    const response = await fetch(`${base}/api/v1/capture`, {
      method: "POST",
      headers: {
        "content-type": "application/json",
        authorization: `Bearer ${bearer}`,
        "idempotency-key": crypto.createHash("sha256").update(`${client}\n${session}\n${body}`).digest("hex"),
      },
      body,
      signal: AbortSignal.timeout(Number(process.env.AICORTEX_HOOK_TIMEOUT_MS ?? 5000)),
    });
    if (!response.ok) record(`${client}: capture answered HTTP ${response.status}`);
  } catch (error) {
    record(`${client}: capture failed: ${error?.message ?? error}`);
  }
}

export async function readStdin() {
  const chunks = [];
  for await (const chunk of process.stdin) chunks.push(chunk);
  return Buffer.concat(chunks).toString("utf8");
}

// Parse JSON without ever throwing: a hook given something unreadable sends nothing.
export function parseJson(text) {
  try {
    return JSON.parse(text);
  } catch {
    return undefined;
  }
}

export function message(role, text) {
  return { event: "message", role, text };
}
