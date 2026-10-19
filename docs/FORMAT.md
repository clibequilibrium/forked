# forked on-disk format, version 1

This document specifies how forked stores agent state. Any implementation
that follows it can read and write the same data as `forked-core`, on any
backend. The format is deliberately tiny: two kinds of records (immutable
**blobs** addressed by hash, and small mutable **refs** moved by
compare-and-swap) and two kinds of objects inside blobs (**nodes** and
**commits**).

## 1. Storage model

A backend must provide:

| Operation | Contract |
|---|---|
| `put_blob(hash, bytes)` | Store immutable bytes under their SHA-256. Idempotent: re-putting an existing hash is a no-op. |
| `get_blob(hash)` | Return the bytes, or absent. |
| `get_ref(name)` | Return the 32-byte hash a ref points at, or absent. |
| `cas_ref(name, expected, new)` | Atomically set `name` to `new` **iff** its current value equals `expected` (`expected = absent` means "create; fail if present"). Of concurrent attempts with equal `expected`, at most one succeeds. |
| `list_refs(prefix)` | Enumerate `(name, hash)` pairs whose name starts with `prefix`. Read-only; order unspecified. |

The first four are the two fundamental operations (blob put/get, ref CAS).
`list_refs` exists so the engine can enumerate branches and anchors without
maintaining a separate index object; every practical backend (SQL table,
KV store, object storage with prefix listing, Durable Object storage)
supports it directly.

Blobs are never modified or required to be deleted. Unreachable (orphaned)
blobs are permitted at all times, e.g. nodes written by a commit that then
lost its CAS, and may be garbage-collected by walking reachability from
all refs.

## 2. Hashing and encoding

- Hash function: **SHA-256**. A hash is 32 raw bytes; rendered as lowercase
  hex (64 chars) where text is needed.
- An object's identity is `SHA-256(encoded bytes)`. The bytes stored under a
  hash must hash to it; adapters may verify on `put_blob`.
- Objects are encoded as **deterministic CBOR** (RFC 8949):
  - Maps and arrays use definite lengths.
  - Integers use the shortest encoding that fits the value.
  - Object fields appear in exactly the order given in §3, no more, no
    fewer, never reordered.
  - Maps inside *state values* (JSON objects) are encoded with keys in
    ascending lexicographic byte order. (`forked-core` guarantees this by
    storing JSON objects in sorted maps.)
  - Hashes are CBOR byte strings (major type 2, length 32).
  - State values are the standard CBOR embedding of JSON: null, bool,
    integer, float (64-bit, shortest lossless float encoding permitted on
    write by the reference implementation, see note), text string, array,
    map with text keys.

Determinism is the invariant that matters: **the same logical object must
always encode to the same bytes**, so equal states have equal hashes and
structural sharing works across writers. The reference encoder is
`ciborium`; its byte output for the structures in §3 is normative for v1.

> Note on floats: the reference implementation may emit the shortest
> lossless float width. Integral JSON numbers are encoded as CBOR integers,
> not floats.

Every object is a CBOR map whose first field is `t`, the object-kind tag:
`"n"` for a tree node, `"c"` for a commit. Readers must reject an object
whose tag does not match the expected kind.

## 3. Objects

### 3.1 Node (`t = "n"`)

One node of the state tree (a hash-array-mapped trie, §4).

| # | Field | Type | Meaning |
|---|---|---|---|
| 1 | `t` | text `"n"` | object kind |
| 2 | `b` | uint (u32) | bitmap: bit *i* set ⇔ a slot exists for chunk value *i* |
| 3 | `s` | array | slots, in ascending chunk-index order; length = popcount(`b`) |

Each slot is a CBOR map, one of:

- **Entry**, `{"k": <key: text>, "v": <value>}`: a key/value pair stored
  inline.
- **Child**, `{"c": <hash: bytes32>}`: pointer to a child node one level
  deeper.

The empty map is the node `{t:"n", b:0, s:[]}`; every session starts from
it, so its hash is the same everywhere.

### 3.2 Commit (`t = "c"`)

| # | Field | Type | Meaning |
|---|---|---|---|
| 1 | `t` | text `"c"` | object kind |
| 2 | `p` | bytes32 or null | parent commit hash; null only for a session's root commit |
| 3 | `r` | bytes32 | hash of the root node of the state tree |
| 4 | `m` | map text→value | caller metadata, keys in ascending order |
| 5 | `e` | array of effect records | side effects recorded atomically with this commit |

An **effect record** is a CBOR map:

| # | Field | Type | Meaning |
|---|---|---|---|
| 1 | `k` | text | idempotency key, unique per session |
| 2 | `d` | text | effect kind (free-form, e.g. `"email.send"`) |
| 3 | `p` | value | the input that produced the effect |
| 4 | `r` | value | the observed result, returned verbatim on replay |

Commits are content-addressed like everything else. Two byte-identical
commits are *the same commit*, e.g. the root commits of any two sessions
coincide. Core does not timestamp commits (that would break this
determinism); put timestamps in `m` if you want them.

## 4. The state tree

State is a map from string keys to JSON values, stored as a HAMT:

- A key's position is determined by `SHA-256(key bytes)`, consumed as 5-bit
  chunks in big-endian bit order: chunk *d* is bits `[5d, 5d+5)` of the
  256-bit digest (zero-padded past bit 255). Fanout is 32; the maximum
  depth is 51, beyond which two distinct keys would constitute a SHA-256
  collision and the tree is rejected as corrupt.
- A node holds, for each set bit in `b`, either an inline **entry** or a
  **child** pointer. Lookup descends by chunk until it hits an entry
  (compare full keys) or an absent bit.
- An update rewrites only the nodes on the path from the touched slot to
  the root; all other subtrees are referenced by hash unchanged. This is
  what makes forking and small commits O(1)-ish and diffs skippable.

### Canonical shape

The tree's shape, and therefore the root hash, is a pure function of its
contents, independent of operation order. Writers must maintain:

1. **Entry depth**: an entry lives at the shallowest depth at which its
   chunk is not shared with any other key in the map. Inserting a key that
   collides with an existing entry's chunk pushes *both* down one level
   (repeatedly, while their chunks collide), creating single-child chain
   nodes as needed.
2. **Collapse on delete**: after a removal, any non-root node left with
   exactly one slot that is an *entry* is replaced in its parent by that
   entry. Collapses propagate upward. (A node whose single slot is a
   *child* is left alone, its subtree's keys still collide at that depth.)
3. The root node is never collapsed and may hold 0 or 1 slots.

Readers must reject a node whose `popcount(b) ≠ len(s)`.

## 5. Refs

Refs are named pointers to commit hashes. forked-core uses this layout
(name components are validated to `[A-Za-z0-9._-]{1,128}`, so `/` is
unambiguous):

| Name | Meaning |
|---|---|
| `s/<session>/b/<branch>` | a branch head |
| `s/<session>/a/<commit-hex>` | an **anchor**: a head abandoned by rewind, kept reachable |

- **Create session** = write the empty root node, write a root commit over
  it, then `cas(s/<id>/b/main, absent → commit)`.
- **Commit** = write new nodes + the new commit blob, then
  `cas(branch, old-head → new-commit)`. The CAS is the *only* point of
  coordination; losing it orphans the new blobs (harmless) and the caller
  retries from the new head. Because effect records are inside the commit
  object, "commit state + record effects" is atomic on every backend by
  construction.
- **Fork** = `cas(s/<id>/b/<new>, absent → source-head)`. One ref write;
  no state copied.
- **Rewind (checkout)** = first `cas(s/<id>/a/<old-head-hex>, absent →
  old-head)` to anchor the history being left behind (idempotent; a crash
  after this step leaves a harmless extra anchor), then
  `cas(branch, old-head → target)`. The target must be a commit reachable
  from the session's refs.

## 6. Effects

Effect records document side effects on the outside world (emails,
payments, API calls). The contract:

- Effects are **never undone by rewind**: anchors keep every recorded
  effect reachable forever.
- An effect's idempotency key is unique per **session** (not per branch) -
  the outside world does not care which branch sent the email. Lookup
  searches every commit reachable from all of the session's branch and
  anchor refs.
- Recording an effect whose key already exists returns the stored record
  (including its result) instead of re-running the effect.
- The existence check and the recording commit are two steps; a strict
  exactly-once guarantee additionally requires writers to a session to be
  serialized (e.g. one Durable Object per session). With concurrent
  writers, the branch-ref CAS still prevents duplicate records *on the
  same branch*; cross-branch duplicates are possible and resolved by
  first-found on lookup.

## 7. Versioning

This is format v1. There is no version byte inside objects; the format is
identified by context (deployment). Breaking changes will introduce new
object tags or a new ref namespace and a migration note here.
