# @forked-run/client

TypeScript client for [forked](https://github.com/clibequilibrium/forked) -
versioned state for AI agents: fork, rewind and replay structured agent
state, with an effect log that keeps side effects effectively-once across
rewinds.

```sh
npm install @forked-run/client
```

```ts
import { ForkedClient } from "@forked-run/client";

// Self-hosted or forked cloud, same API, switch via baseUrl.
const forked = new ForkedClient({
  baseUrl: "http://localhost:8080", // or https://api.forked.run
  apiKey: process.env.FORKED_API_KEY, // fk_live_… on cloud
});

const session = await forked.createSession();
const main = session.branch();

// Commit state like code.
await main.put({ plan: ["draft", "review", "send"], step: 1 });

// Fork to explore an alternative, O(1).
const alt = await main.fork("alternative");
await alt.put({ step: 2 });

// Rewind exactly; effects are never undone.
const history = await main.log();
await main.checkout(history[1]!.hash);

// Record side effects idempotently.
const outcome = await session.recordEffect({
  key: "send-email:42",
  kind: "email.send",
  payload: { to: "user@example.com" },
  result: { messageId: "abc" },
});
if (outcome.status === "already_recorded") {
  // Already happened (even on a rewound branch), reuse the stored result.
  console.log(outcome.record.result);
}

// Diff two commits.
console.log(await session.diff(history[1]!.hash, history[0]!.hash));
```

Conflicts from optimistic concurrency throw `ForkedError` with
`isConflict === true`, re-read the head and retry:

```ts
const head = await main.head();
try {
  await main.put({ step: 3 }, { expectedHead: head });
} catch (e) {
  if (e instanceof ForkedError && e.isConflict) {
    // branch moved, re-read and retry
  } else throw e;
}
```

The full HTTP API is documented in
[docs/API.md](https://github.com/clibequilibrium/forked/blob/main/docs/API.md).

Apache-2.0.
