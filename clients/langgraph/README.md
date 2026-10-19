# @forked/langgraph

A [LangGraph](https://github.com/langchain-ai/langgraphjs) checkpointer
backed by [forked](https://github.com/clibequilibrium/forked): every checkpoint
is a commit in a versioned, branchable session.

What you get over a plain checkpointer:

- **Time travel is a real rewind.** Resuming from an older checkpoint moves
  the branch head back; the abandoned run stays reachable (anchored) and the
  forked console shows the fork point in the branch tree.
- **Readable state.** JSON-safe channel values are mirrored into state keys,
  so the console shows your agent's actual state at every step, diffable
  between any two checkpoints.
- **Effectively-once side effects.** Record emails/payments/API calls through
  forked's effect log; a rewound-and-replayed graph sees
  `already_recorded` and reuses the stored result instead of re-sending.

```sh
npm install @forked/langgraph @forked/client @langchain/langgraph
```

```ts
import { ForkedClient } from "@forked/client";
import { ForkedSaver } from "@forked/langgraph";
import { StateGraph } from "@langchain/langgraph";

const checkpointer = new ForkedSaver({
  client: new ForkedClient({
    baseUrl: "http://localhost:8080", // or https://api.forked.run
    apiKey: process.env.FORKED_API_KEY,
  }),
});

const graph = workflow.compile({ checkpointer });
await graph.invoke(input, { configurable: { thread_id: "thread-1" } });

// Time travel exactly as LangGraph documents it, forked turns it into a
// real branch rewind:
const history = [];
for await (const state of graph.getStateHistory({ configurable: { thread_id: "thread-1" } })) {
  history.push(state);
}
await graph.invoke(null, history[2].config); // resume from an earlier step
```

Mapping: `thread_id` → session, `checkpoint_ns` → branch, checkpoint →
commit. Thread ids outside `[A-Za-z0-9._-]{1,128}` are hashed into a valid
session name.

`examples/smoke.mjs` runs the full flow against a local server:

```sh
forked-server --db memory --addr 127.0.0.1:8099
node examples/smoke.mjs
```

Apache-2.0.
