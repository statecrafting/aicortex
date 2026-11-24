// A real MCP client for spec 021 AC-2: the official TypeScript SDK's client
// over streamable HTTP. It is not a test of the SDK; it is the evidence that a
// client this product did not write speaks to the server.
//
//   MCP_URL    the server, for example http://127.0.0.1:PORT/mcp (required)
//   MCP_TOKEN  an access token to present from the first request. Without it
//              the client opens anonymously, which is where the bootstrap of
//              B-4 begins, and the process exits 3 with what the server said.
//   MCP_SDK    the directory holding node_modules/@modelcontextprotocol/sdk
//
// On success it prints one JSON line: the SDK's version, the negotiated
// protocol revision, the tool names, the id it captured, and whether the
// memory read back through fetch carries the framing statement.
import fs from "node:fs";
import { pathToFileURL } from "node:url";
import path from "node:path";

const sdkDir = process.env.MCP_SDK ?? process.cwd();
const root = path.join(sdkDir, "node_modules/@modelcontextprotocol/sdk");
const load = (file) => import(pathToFileURL(path.join(root, "dist/esm", file)).href);
const { Client } = await load("client/index.js");
const { StreamableHTTPClientTransport } = await load("client/streamableHttp.js");
const version = JSON.parse(fs.readFileSync(path.join(root, "package.json"), "utf8")).version;

const url = new URL(process.env.MCP_URL);
const token = process.env.MCP_TOKEN;
const transport = new StreamableHTTPClientTransport(url, {
  requestInit: token ? { headers: { Authorization: `Bearer ${token}` } } : undefined,
});
const client = new Client({ name: "aicortex-ac2-sdk-client", version: "1" });

try {
  await client.connect(transport);
} catch (error) {
  console.error(`connect failed: ${error?.constructor?.name}: ${error?.message}`);
  process.exit(3);
}

const text = (result) => JSON.parse(result.content[0].text);
const tools = (await client.listTools()).tools.map((tool) => tool.name);
const note = `an sdk client remembers ${Date.now()}`;
const captured = await client.callTool({
  name: "capture",
  arguments: { candidates: [{ kind: "observation", text: note }] },
});
const id = text(captured).results[0].id;
const fetched = await client.callTool({ name: "fetch", arguments: { id } });
const body = fetched.structuredContent ?? text(fetched);
const resources = (await client.listResources()).resources.map((r) => r.uri);
await client.close();
console.log(
  JSON.stringify({
    sdk: `@modelcontextprotocol/sdk ${version}`,
    tools,
    resources,
    id,
    isError: fetched.isError === true,
    framed: typeof body.framing === "string" && body.memories.some((m) => m.includes(note)),
  }),
);
