# @forked-run/mcp

**Give your agent undo.** An MCP server that exposes
[forked](https://github.com/clibequilibrium/forked), versioned agent state, as
tools, so Claude Code, Claude Desktop, Cursor or any MCP client can
checkpoint, fork and **rewind its own working state**, and keep side
effects effectively-once across retries. No SDK integration required.

## Setup

Against a local server (`forked-server --db forked.db`):

```sh
# Claude Code
claude mcp add forked --env FORKED_URL=http://localhost:8080 -- npx -y @forked-run/mcp
```

Against forked cloud:

```sh
claude mcp add forked \
  --env FORKED_URL=https://api.forked.run \
  --env FORKED_API_KEY=fk_live_… \
  -- npx -y @forked-run/mcp
```

Claude Desktop / Cursor (`mcpServers` JSON):

```json
{
  "mcpServers": {
    "forked": {
      "command": "npx",
      "args": ["-y", "@forked-run/mcp"],
      "env": { "FORKED_URL": "http://localhost:8080" }
    }
  }
}
```

## Tools

| Tool | What the agent uses it for |
|---|---|
| `forked_create_session` | one session per task |
| `forked_save` | checkpoint state before risky steps |
| `forked_load` | read current state |
| `forked_history` | list checkpoints (hashes + messages + effects) |
| `forked_rewind` | **undo**: go back N checkpoints or to a hash, exact, never destructive |
| `forked_fork` | explore an alternative on a new branch |
| `forked_diff` | compare two checkpoints |
| `forked_find_effect` | "did I already send this email?", probe before acting |
| `forked_record_effect` | log a performed side effect under an idempotency key |

The server's instructions teach the agent the habits: checkpoint before
risk, rewind on failure, probe the effect log before touching the outside
world.

`examples/smoke.mjs` drives the full loop over real MCP stdio:

```sh
forked-server --db memory --addr 127.0.0.1:8091
node examples/smoke.mjs
```

Apache-2.0.
