# forked examples

Three runnable scripts, each demonstrating one core use case against a real
forked server. They simulate the "agent" with plain code (no LLM calls), so
the mechanics are easy to read.

| Script | Use case | forked feature |
|---|---|---|
| `01-safe-retry.mjs` | an agent crashes mid-run and is re-run safely: no double charge, no duplicate email | effect log (effectively-once) |
| `02-time-travel-debugging.mjs` | find exactly which step corrupted state, fix it, keep the bad run auditable | log + structural diff + rewind |
| `03-best-of-n.mjs` | explore 12 candidate plans from identical state, promote the winner | O(1) fork + checkout |

## Run

```sh
# from the repo root: start a server (any backend)
cargo run -p forked-server -- --db memory --addr 127.0.0.1:8080

# in another terminal
cd examples
npm install
npm run 01     # or 02, 03, all
```

Every script also runs unchanged against forked cloud:

```sh
FORKED_URL=https://api.forked.run FORKED_API_KEY=fk_live_… npm run 01
```

After running, open the session in the console to see the branch tree -
example 2 leaves an anchored "bad" history, example 3 leaves a 12-way fan.
