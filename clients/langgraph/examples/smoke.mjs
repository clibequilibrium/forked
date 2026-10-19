// End-to-end smoke test against a running forked server:
//   forked-server --db memory --addr 127.0.0.1:8099
//   node examples/smoke.mjs
// Exercises the raw SDK, then the LangGraph checkpointer including real
// time travel (rewind + anchored history).
import assert from "node:assert/strict";
import { ForkedClient, ForkedError } from "@forked/client";
import { ForkedSaver } from "../dist/index.js";

const baseUrl = process.env.FORKED_URL ?? "http://127.0.0.1:8099";
const forked = new ForkedClient({ baseUrl });

// --- raw SDK -----------------------------------------------------------------
await forked.health();
const session = await forked.createSession();
const main = session.branch();

const first = await main.put({ plan: ["draft", "review"], step: 1 });
await main.put({ step: 2, notes: { tone: "friendly" } });

const alt = await main.fork("alt");
await alt.put({ step: 99 });
assert.equal((await main.state()).state.step, 2);
assert.equal((await alt.state()).state.step, 99);

// optimistic concurrency: stale head conflicts
let conflicted = false;
try {
  await main.put({ step: 3 }, { expectedHead: first });
} catch (e) {
  conflicted = e instanceof ForkedError && e.isConflict;
}
assert.ok(conflicted, "stale expectedHead must 409");

// rewind restores exact state; effects survive
await session.recordEffect({
  key: "email-1",
  kind: "email.send",
  result: { id: "msg-1" },
});
await main.checkout(first);
assert.equal((await main.state()).state.step, 1);
const replay = await session.recordEffect({ key: "email-1", kind: "email.send" });
assert.equal(replay.status, "already_recorded");
assert.equal(replay.record.result.id, "msg-1");

const diff = await session.diff(first, (await alt.state()).head);
assert.ok(diff.some((d) => d.key === "step" && d.change === "modified"));

const graph = await session.graph();
assert.ok(graph.branches.length === 2 && graph.anchors.length >= 1);
console.log(
  `✓ SDK: session ${session.id}, ${graph.commits.length} commits, ` +
    `${graph.branches.length} branches, ${graph.anchors.length} anchor(s)`,
);

// --- LangGraph checkpointer ----------------------------------------------------
const saver = new ForkedSaver({ client: forked });
const threadId = `smoke-${Date.now()}`;
const cfg = (checkpointId) => ({
  configurable: { thread_id: threadId, checkpoint_ns: "", checkpoint_id: checkpointId },
});
const mkCheckpoint = (id, values) => ({
  v: 4,
  id,
  ts: new Date().toISOString(),
  channel_values: values,
  channel_versions: { messages: 1 },
  versions_seen: {},
});
const mkMeta = (step) => ({ source: "loop", step, parents: {} });

// three checkpoints in a row
const c1 = await saver.put(cfg(undefined), mkCheckpoint("cp-1", { messages: ["hi"] }), mkMeta(1), {});
const c2 = await saver.put(c1, mkCheckpoint("cp-2", { messages: ["hi", "plan"] }), mkMeta(2), {});
await saver.put(c2, mkCheckpoint("cp-3", { messages: ["hi", "plan", "oops"] }), mkMeta(3), {});

// pending writes attach to cp-2
await saver.putWrites(c2, [["messages", { delta: "tool-result" }]], "task-7");

// latest tuple is cp-3
const latest = await saver.getTuple(cfg(undefined));
assert.equal(latest.checkpoint.id, "cp-3");
assert.equal(latest.parentConfig.configurable.checkpoint_id, "cp-2");

// fetch cp-2 explicitly: checkpoint + metadata + pending writes round-trip
const tuple2 = await saver.getTuple(cfg("cp-2"));
assert.equal(tuple2.checkpoint.id, "cp-2");
assert.deepEqual(tuple2.checkpoint.channel_values.messages, ["hi", "plan"]);
assert.equal(tuple2.metadata.step, 2);
assert.equal(tuple2.pendingWrites.length, 1);
assert.deepEqual(tuple2.pendingWrites[0], ["task-7", "messages", { delta: "tool-result" }]);

// list newest-first with limit
const listed = [];
for await (const t of saver.list(cfg(undefined), { limit: 2 })) listed.push(t.checkpoint.id);
assert.deepEqual(listed, ["cp-3", "cp-2"]);

// time travel: resume from cp-2 → real rewind, cp-3 history is anchored
await saver.put(cfg("cp-2"), mkCheckpoint("cp-4", { messages: ["hi", "plan", "better"] }), mkMeta(3), {});
const afterTravel = await saver.getTuple(cfg(undefined));
assert.equal(afterTravel.checkpoint.id, "cp-4");
assert.equal(afterTravel.parentConfig.configurable.checkpoint_id, "cp-2");

// thread ids matching [A-Za-z0-9._-] map to the session of the same name
const g2 = await forked.session(threadId).graph();
assert.ok(g2.anchors.length >= 1, "time travel must anchor the abandoned head");
console.log(
  `✓ checkpointer: thread ${threadId}, ${g2.commits.length} commits, ` +
    `${g2.anchors.length} anchor(s) after time travel`,
);
console.log("ALL SMOKE TESTS PASSED");
