// Use case 3, best-of-N: tree search over agent trajectories.
//
// From one shared state, fork N candidate branches, let each explore a
// different strategy, score the outcomes, and promote the winner back to
// main. Forking is O(1), a fork writes one ref and copies nothing, so
// exploring 12 futures costs 12 ref writes, not 12 copies of the state.
//
//   cargo run -p forked-server -- --db memory --addr 127.0.0.1:8080
//   node 03-best-of-n.mjs

import assert from "node:assert/strict";
import { ForkedClient } from "@forked/client";

const forked = new ForkedClient({
  baseUrl: process.env.FORKED_URL ?? "http://127.0.0.1:8080",
  apiKey: process.env.FORKED_API_KEY,
});

const N = 12;
const STRATEGIES = ["bullet-points", "narrative", "faq", "case-study"];
const TONES = ["technical", "executive", "casual"];

// Deterministic stand-ins for "run the agent" and "run the judge".
const mulberry32 = (seed) => () => {
  seed |= 0;
  seed = (seed + 0x6d2b79f5) | 0;
  let t = Math.imul(seed ^ (seed >>> 15), 1 | seed);
  t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
  return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
};
const draftWith = (strategy, tone, rand) =>
  `[${strategy}/${tone}] intro… ${Math.round(rand() * 900 + 300)} words…`;
const judge = (strategy, tone, rand) =>
  Math.round(
    (STRATEGIES.indexOf(strategy) === 2 ? 70 : 50) + // judge likes FAQs
      (tone === "executive" ? 10 : 0) +
      rand() * 25,
  );

// ---------------------------------------------------------------------------
const session = await forked.createSession();
const main = session.branch();
console.log(`session: ${session.id}\n`);

console.log("── seed the shared context on main ─────────────────────");
await main.put(
  {
    task: "write launch announcement for forked 0.1",
    "context/audience": "AI engineers",
    "context/key-points": ["fork O(1)", "rewind", "effectively-once effects"],
  },
  { meta: { message: "seeded task + context" } },
);

console.log(`── fork ${N} candidates from identical state ─────────────`);
const t0 = performance.now();
const candidates = [];
for (let i = 0; i < N; i += 1) {
  candidates.push(await main.fork(`candidate-${String(i).padStart(2, "0")}`));
}
const forkMs = performance.now() - t0;
console.log(`  ${N} forks in ${forkMs.toFixed(0)}ms, one ref write each, zero state copied\n`);

console.log("── each candidate explores a different strategy ────────");
const scored = [];
for (const [i, branch] of candidates.entries()) {
  const rand = mulberry32(i * 2654435761);
  const strategy = STRATEGIES[i % STRATEGIES.length];
  const tone = TONES[i % TONES.length];

  await branch.put(
    { "plan/strategy": strategy, "plan/tone": tone },
    { meta: { message: `chose ${strategy}/${tone}` } },
  );
  await branch.put({ draft: draftWith(strategy, tone, rand) }, { meta: { message: "drafted" } });
  const score = judge(strategy, tone, rand);
  const head = await branch.commit([{ op: "put", key: "score", value: score }], {
    meta: { message: `judged: ${score}` },
  });
  scored.push({ branch, strategy, tone, score, head });
  console.log(`  ${branch.name}  ${strategy.padEnd(13)} ${tone.padEnd(10)} → score ${score}`);
}

const winner = scored.reduce((a, b) => (b.score > a.score ? b : a));
console.log(`\n── promote the winner to main ──────────────────────────`);
console.log(`  winner: ${winner.branch.name} (${winner.strategy}/${winner.tone}, score ${winner.score})`);

// The winner's head is a commit in this session, so main can check out to
// it directly, promotion is a rewind-forward, not a copy.
await main.checkout(winner.head);
const promoted = (await main.state()).state;
assert.equal(promoted.score, winner.score);
assert.equal(promoted["plan/strategy"], winner.strategy);
console.log(`  main now carries the winning draft (score ${promoted.score})`);

const graph = await session.graph();
assert.equal(graph.branches.length, N + 1);
console.log(
  `\n✓ explored ${N} futures for ${N} ref writes: ${graph.branches.length} branches, ` +
    `${graph.commits.length} commits, open session ${session.id} in the console to see the fan`,
);
