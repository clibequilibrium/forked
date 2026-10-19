# forked HTTP API (v1)

One API, two deployments:

- **Self-hosted** (`forked-server`): `http://localhost:8080`, optional
  static bearer token (`FORKED_TOKEN`).
- **forked cloud**: `https://api.forked.run`, API key per account
  (`Authorization: Bearer fk_live_…`).

Both are fed through the same dispatcher (`forked-api`), so routes, bodies
and error shapes are identical by construction. Switch between them by
changing `baseUrl`.

All bodies are JSON. Hashes are 64-char lowercase hex. Session and branch
names match `[A-Za-z0-9._-]{1,128}`.

## Errors

Non-2xx responses carry:

```json
{ "error": { "code": "conflict", "message": "…" } }
```

| Status | Code | Meaning |
|---|---|---|
| 400 | `invalid_request` | bad name, hash, JSON or query parameter |
| 401 | `unauthorized` | missing/invalid credentials |
| 404 | `not_found` | session, branch or commit does not exist |
| 404 | `unknown_route` | no such endpoint |
| 409 | `conflict` | branch moved concurrently (re-read head, retry) |
| 409 | `session_exists` / `branch_exists` | create collided |
| 413 | `payload_too_large` | body over 16 MiB |
| 429 | `rate_limited` | cloud only: plan limit reached |
| 500 | `internal` / `storage_error` | server-side failure |

## Endpoints

### `GET /v1/health`
→ `{ "status": "ok" }`

### `POST /v1/sessions`
Create a session with an empty state tree and a `main` branch.

Body (optional): `{ "session_id": "my-run-42" }`, omit to get a generated id.

→ `{ "session_id": "…", "branch": "main", "head": "<hash>" }`

### `GET /v1/sessions/{id}/branches`
→ `{ "branches": [ { "name": "main", "head": "<hash>" } ] }`

### `GET /v1/sessions/{id}/branches/{branch}/state`
Materialize the branch's full state.

→ `{ "head": "<hash>", "state": { "key": <value>, … } }`

### `POST /v1/sessions/{id}/branches/{branch}/commit`

```json
{
  "ops": [
    { "op": "put", "key": "plan", "value": ["draft", "send"] },
    { "op": "delete", "key": "scratch" }
  ],
  "meta": { "message": "optional metadata" },
  "effects": [],
  "expected_head": "<hash, optional>"
}
```

`expected_head` enables optimistic concurrency: if the branch has moved,
nothing is written and you get `409 conflict`. Omit it to commit on the
current head. `effects` records effect entries atomically with the commit
**without** an idempotency check, use `/effects` when you want
effectively-once semantics.

→ `{ "head": "<new hash>" }`

### `POST /v1/sessions/{id}/branches/{branch}/fork`
Body: `{ "branch": "experiment" }` → `{ "branch": "experiment", "head": "<hash>" }`

O(1): writes one ref, copies nothing.

### `POST /v1/sessions/{id}/branches/{branch}/checkout`
Rewind the branch to an earlier commit of the same session.

Body: `{ "commit": "<hash>" }` → `{ "branch": "…", "head": "<hash>" }`

History past the rewind point stays reachable (anchored); effects recorded
there remain visible to `/effects`.

### `GET /v1/sessions/{id}/log?branch=main&limit=50`
Newest first. → `{ "entries": [ Commit, … ] }` where `Commit` is:

```json
{ "hash": "…", "parent": "…|null", "root": "…", "meta": {}, "effects": [] }
```

### `GET /v1/sessions/{id}/diff?a=<hash>&b=<hash>`
Structural diff of two commits' states (shared subtrees are skipped by
hash).

→ `{ "entries": [ { "key": "step", "change": "modified", "old": 1, "new": 2 } ] }`

`change` ∈ `added` (only `new`), `removed` (only `old`), `modified` (both).

### `GET /v1/sessions/{id}/graph`
The whole session for visualization: every branch, rewind anchor, and
reachable commit.

→ `{ "branches": [{name, head}], "anchors": ["<hash>"], "commits": [Commit, …] }`

### `POST /v1/sessions/{id}/effects`
Record a side effect idempotently (session-wide, across branches and
rewinds).

```json
{
  "branch": "main",
  "key": "send-email:42",
  "kind": "email.send",
  "payload": { "to": "user@example.com" },
  "result": { "message_id": "abc" }
}
```

→ first time: `{ "status": "recorded", "record": {…}, "commit": "<hash>" }`
→ replay: `{ "status": "already_recorded", "record": {…} }` with the
**original** stored result, don't re-run the effect, use `record.result`.

### `GET /v1/sessions/{id}/effects?key=<urlencoded key>`
Look up an effect by idempotency key without recording anything.

→ `{ "record": { "key", "kind", "payload", "result" } }` or `{ "record": null }`

The effectively-once recipe for performing a side effect (strict when a
session has one writer at a time, forked cloud serializes writers per
session; see FORMAT.md §6 for the concurrent-writer caveat):

1. `GET /effects?key=…`, if a record exists, **reuse its `result`, do not
   re-run the work**.
2. Otherwise do the work.
3. `POST /effects` with the observed result. If a concurrent writer beat
   you to it, the response is `already_recorded` with *their* stored
   result, use that one.
