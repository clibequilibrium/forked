// Use case 2, time travel debugging: "what did the agent know at step N?"
//
// A research agent computes a quote in USD. The final answer is wrong.
// Instead of eyeballing a transcript, we walk the commit log and diff each
// step against its parent, the structural diff pinpoints the exact step
// (and tool call) that poisoned the state. Then we rewind to just before
// it, re-run from there with the fixed tool, and the bad run stays
// anchored in history for the postmortem.
//
//   cargo run -p forked-server -- --db memory --addr 127.0.0.1:8080
//   node 02-time-travel-debugging.mjs

import assert from "node:assert/strict";
import { ForkedClient } from "@forked/client";

const forked = new ForkedClient({
  baseUrl: process.env.FORKED_URL ?? "http://127.0.0.1:8080",
  apiKey: process.env.FORKED_API_KEY,
});

// Two versions of a "tool": the buggy one returns a GBP rate when asked
// for EUR→USD.
const fxToolBuggy = async () => ({ pair: "EUR/USD", rate: 1.27 }); // actually GBP/USD!
const fxToolFixed = async () => ({ pair: "EUR/USD", rate: 1.08 });

async function quoteSteps(main, fxTool) {
  await main.put(
    { "quote/base_eur": 25000 },
    { meta: { message: "read base price from catalog", tool: "catalog.lookup" } },
  );
  const fx = await fxTool();
  await main.put(
    { "quote/fx_rate": fx.rate, "quote/fx_pair": fx.pair },
    { meta: { message: "fetched FX rate", tool: "fx.lookup" } },
  );
  const { state } = await main.state();
  await main.put(
    { "quote/total_usd": Math.round(state["quote/base_eur"] * state["quote/fx_rate"]) },
    { meta: { message: "computed USD total" } },
  );
}

// ---------------------------------------------------------------------------
const session = await forked.createSession();
const main = session.branch();
console.log(`session: ${session.id}\n`);

console.log("── the agent runs (with a buggy FX tool) ───────────────");
await quoteSteps(main, fxToolBuggy);
const badTotal = (await main.state()).state["quote/total_usd"];
console.log(`  agent's answer: $${badTotal.toLocaleString()}  ← customer says this is wrong\n`);

console.log("── debugging: diff every step against its parent ───────");
const log = await main.log(); // newest first
const steps = [...log].reverse(); // oldest → newest
let culpritIndex = -1;
for (let i = 1; i < steps.length; i += 1) {
  const diff = await session.diff(steps[i - 1].hash, steps[i].hash);
  const summary = diff
    .map((d) => `${d.key}: ${JSON.stringify(d.old ?? "∅")} → ${JSON.stringify(d.new ?? "∅")}`)
    .join(", ");
  const meta = steps[i].meta;
  console.log(`  step ${i} [${meta.message ?? ""}] ${summary || "(no state change)"}`);

  // The hunt: which step set an implausible EUR/USD rate?
  const rateChange = diff.find((d) => d.key === "quote/fx_rate");
  if (rateChange && (rateChange.new > 1.2 || rateChange.new < 0.9)) {
    culpritIndex = i;
    console.log(
      `       ^^^ CULPRIT, tool "${meta.tool}" returned rate ${rateChange.new}, outside the plausible EUR/USD band`,
    );
  }
}
assert.ok(culpritIndex > 0, "diff walk must find the poisoned step");

console.log("\n── fix: rewind to just before the culprit, re-run ──────");
const beforeCulprit = steps[culpritIndex - 1].hash;
await main.checkout(beforeCulprit);
console.log(`  rewound main to ${beforeCulprit.slice(0, 8)} (state is byte-exact as of that step)`);

// Re-run only the remaining steps, with the fixed tool.
const fx = await fxToolFixed();
await main.put(
  { "quote/fx_rate": fx.rate, "quote/fx_pair": fx.pair },
  { meta: { message: "fetched FX rate (tool fixed)", tool: "fx.lookup@v2" } },
);
const { state } = await main.state();
await main.put(
  { "quote/total_usd": Math.round(state["quote/base_eur"] * state["quote/fx_rate"]) },
  { meta: { message: "recomputed USD total" } },
);

const goodTotal = (await main.state()).state["quote/total_usd"];
console.log(`  corrected answer: $${goodTotal.toLocaleString()}`);
assert.equal(goodTotal, Math.round(25000 * 1.08));

const graph = await session.graph();
assert.ok(graph.anchors.length >= 1, "the bad run must stay anchored");
console.log(
  `\n✓ found the poisoned step by diffing, fixed it by rewinding; the bad run is still` +
    ` auditable (${graph.anchors.length} anchor, ${graph.commits.length} commits), session ${session.id}`,
);
