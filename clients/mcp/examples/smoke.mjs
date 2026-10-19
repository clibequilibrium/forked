// End-to-end MCP smoke test: spawns the forked-mcp CLI over stdio and
// drives it with the real MCP client, checkpoint, rewind, effect probe.
//
//   forked-server --db memory --addr 127.0.0.1:8091
//   node examples/smoke.mjs
import assert from "node:assert/strict";
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";

const transport = new StdioClientTransport({
  command: process.execPath,
  args: ["dist/cli.js"],
  env: { ...process.env, FORKED_URL: process.env.FORKED_URL ?? "http://127.0.0.1:8091" },
});
const client = new Client({ name: "smoke", version: "0.0.1" });
await client.connect(transport);

const tools = await client.listTools();
const names = tools.tools.map((t) => t.name).sort();
console.log(`tools: ${names.join(", ")}`);
assert.ok(names.includes("forked_rewind") && names.includes("forked_find_effect"));

const call = async (name, args) => {
  const res = await client.callTool({ name, arguments: args });
  const text = res.content?.[0]?.text ?? "";
  assert.ok(!res.isError, `${name} failed: ${text}`);
  return JSON.parse(text);
};

// checkpoint → change → rewind
const { session_id } = await call("forked_create_session", {});
await call("forked_save", { session_id, data: { plan: "A", step: 1 }, message: "checkpoint A" });
await call("forked_save", { session_id, data: { plan: "B (risky)", step: 2 }, message: "risky step" });
const rewound = await call("forked_rewind", { session_id, steps: 1 });
assert.equal(rewound.state.plan, "A");

// effect probe → record → probe again
const probe1 = await call("forked_find_effect", { session_id, key: "email:1" });
assert.equal(probe1.already_done, false);
await call("forked_record_effect", {
  session_id,
  key: "email:1",
  kind: "email.send",
  result: { id: "m1" },
});
const probe2 = await call("forked_find_effect", { session_id, key: "email:1" });
assert.equal(probe2.already_done, true);
assert.equal(probe2.record.result.id, "m1");

// history shows the effect commit
const history = await call("forked_history", { session_id, limit: 5 });
assert.ok(history.some((h) => h.effects.includes("email:1")));

await client.close();
console.log(`✓ MCP smoke passed, session ${session_id}: checkpoint, rewind, effect probe/record`);
