//! The persistent, structurally shared state tree.
//!
//! A hash-array-mapped trie (HAMT): keys are addressed by the SHA-256 of
//! the key string, consumed 5 bits at a time (fanout 32). Nodes are
//! content-addressed and immutable; an update rewrites only the path from
//! the touched leaf to the root (O(log₃₂ n) new nodes), sharing every other
//! subtree with the previous version by hash.
//!
//! The shape is canonical: the same set of entries always produces the same
//! tree, and therefore the same root hash, regardless of the order of
//! inserts and deletes. See `docs/FORMAT.md` for the invariants.
//!
//! Everything here is written iteratively (explicit stacks, an index arena
//! for the mutable overlay) rather than with boxed recursive futures, so
//! the generated futures carry no `dyn Future` and remain `Send` whenever
//! the storage backend's futures are `Send`, native servers need that,
//! while wasm backends remain free to be `!Send`.

use std::collections::BTreeMap;
use std::mem;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::encoding::{get_object, put_object};
use crate::{Error, Hash, Result, Storage, Value};

/// Deepest level at which a 5-bit chunk can be taken from a 256-bit hash.
/// Two distinct keys sharing all 52 chunks would be a SHA-256 collision.
const MAX_DEPTH: usize = 51;

/// Object-kind tag for tree nodes.
const TAG_NODE: &str = "n";

/// On-disk form of a tree node.
#[derive(Serialize, Deserialize)]
pub(crate) struct NodeRepr {
    #[serde(rename = "t")]
    tag: String,
    #[serde(rename = "b")]
    bitmap: u32,
    #[serde(rename = "s")]
    slots: Vec<SlotRepr>,
}

/// A slot in a node: either an inline entry or a pointer to a child node.
#[derive(Serialize, Deserialize)]
#[serde(untagged)]
pub(crate) enum SlotRepr {
    Child {
        #[serde(rename = "c")]
        hash: Hash,
    },
    Entry {
        #[serde(rename = "k")]
        key: String,
        #[serde(rename = "v")]
        value: Value,
    },
}

/// Hash a user key into its 256-bit trie path.
fn key_hash(key: &str) -> [u8; 32] {
    Sha256::digest(key.as_bytes()).into()
}

/// Extract the 5-bit chunk at `depth` from a key hash (big-endian bit
/// order). Chunks past bit 255 are zero-padded on the right.
fn chunk(hash: &[u8; 32], depth: usize) -> u32 {
    let bit = depth * 5;
    let byte = bit / 8;
    let shift = bit % 8;
    let hi = hash[byte] as u32;
    let lo = if byte + 1 < 32 {
        hash[byte + 1] as u32
    } else {
        0
    };
    let word = (hi << 8) | lo;
    (word >> (11 - shift)) & 0x1F
}

/// Position of the slot for `bit` within the compressed slot vector.
fn slot_pos(bitmap: u32, bit: u32) -> usize {
    (bitmap & (bit - 1)).count_ones() as usize
}

fn depth_exceeded() -> Error {
    Error::Corrupt("key hash collision exceeds max depth".to_string())
}

async fn load_node<S: Storage>(storage: &S, hash: &Hash) -> Result<NodeRepr> {
    let node: NodeRepr = get_object(storage, hash).await?;
    if node.tag != TAG_NODE {
        return Err(Error::Corrupt(format!(
            "expected node object, found tag {:?}",
            node.tag
        )));
    }
    if node.bitmap.count_ones() as usize != node.slots.len() {
        return Err(Error::Corrupt(
            "node bitmap does not match slot count".to_string(),
        ));
    }
    Ok(node)
}

/// Write the canonical empty root node and return its hash.
pub(crate) async fn put_empty_root<S: Storage>(storage: &S) -> Result<Hash> {
    put_object(
        storage,
        &NodeRepr {
            tag: TAG_NODE.to_string(),
            bitmap: 0,
            slots: Vec::new(),
        },
    )
    .await
}

/// Look up a single key starting from the root node at `root`.
pub(crate) async fn tree_get<S: Storage>(
    storage: &S,
    root: &Hash,
    key: &str,
) -> Result<Option<Value>> {
    let kh = key_hash(key);
    let mut current = *root;
    for depth in 0..=MAX_DEPTH {
        let node = load_node(storage, &current).await?;
        let bit = 1u32 << chunk(&kh, depth);
        if node.bitmap & bit == 0 {
            return Ok(None);
        }
        let pos = slot_pos(node.bitmap, bit);
        match node.slots.into_iter().nth(pos) {
            Some(SlotRepr::Entry { key: k, value }) => {
                return Ok(if k == key { Some(value) } else { None });
            }
            Some(SlotRepr::Child { hash }) => current = hash,
            None => return Err(Error::Corrupt("slot index out of range".to_string())),
        }
    }
    Err(depth_exceeded())
}

/// Collect every entry reachable from the node at `hash` into `out`.
pub(crate) async fn collect_entries<S: Storage>(
    storage: &S,
    hash: &Hash,
    out: &mut BTreeMap<String, Value>,
) -> Result<()> {
    let mut stack = vec![*hash];
    while let Some(h) = stack.pop() {
        let node = load_node(storage, &h).await?;
        for slot in node.slots {
            match slot {
                SlotRepr::Entry { key, value } => {
                    out.insert(key, value);
                }
                SlotRepr::Child { hash } => stack.push(hash),
            }
        }
    }
    Ok(())
}

/// A decoded slot handed to the diff algorithm.
pub(crate) enum NodeSlot {
    /// An inline key/value entry.
    Entry(String, Value),
    /// A pointer to a child subtree.
    Child(Hash),
}

/// Load a node and decompose it into its bitmap and decoded slots, in slot
/// order (ascending chunk index).
pub(crate) async fn load_node_slots<S: Storage>(
    storage: &S,
    hash: &Hash,
) -> Result<(u32, Vec<NodeSlot>)> {
    let node = load_node(storage, hash).await?;
    let slots = node
        .slots
        .into_iter()
        .map(|s| match s {
            SlotRepr::Entry { key, value } => NodeSlot::Entry(key, value),
            SlotRepr::Child { hash } => NodeSlot::Child(hash),
        })
        .collect();
    Ok((node.bitmap, slots))
}

// ---------------------------------------------------------------------------
// Mutable overlay used while building a commit.
// ---------------------------------------------------------------------------

/// A slot in the in-memory overlay: an entry, an untouched stored subtree,
/// or a modified subtree held in the arena.
enum MemSlot {
    Entry { key: String, value: Value },
    Stored(Hash),
    Mem(usize),
}

/// In-memory copy of a node along a modified path. Children under
/// modification are arena indices (`MemSlot::Mem`), which keeps every
/// algorithm an iterative loop instead of recursion over `&mut` borrows.
struct MemNode {
    bitmap: u32,
    slots: Vec<MemSlot>,
}

/// A state tree checked out for modification. Apply any number of inserts
/// and removes, then [`flush`](MemTree::flush) once to write the new nodes
/// and obtain the new root hash. Conflicted or abandoned flushes leave only
/// orphaned blobs, which are harmless (and collectable by a later GC).
pub(crate) struct MemTree {
    nodes: Vec<MemNode>,
    root: usize,
}

/// Outcome of classifying the occupied slot an operation landed on.
enum SlotCase {
    /// The slot holds an entry with exactly this key.
    SameKey,
    /// The slot holds an entry with a different key.
    OtherKey,
    /// The slot points at a stored (not yet loaded) subtree.
    Load(Hash),
    /// The slot points at an arena subtree.
    Descend(usize),
}

impl MemTree {
    /// Load the root node at `root` for modification.
    pub(crate) async fn load<S: Storage>(storage: &S, root: &Hash) -> Result<Self> {
        let repr = load_node(storage, root).await?;
        let mut tree = MemTree {
            nodes: Vec::new(),
            root: 0,
        };
        tree.push_from_repr(repr);
        Ok(tree)
    }

    fn push_empty(&mut self) -> usize {
        self.nodes.push(MemNode {
            bitmap: 0,
            slots: Vec::new(),
        });
        self.nodes.len() - 1
    }

    fn push_from_repr(&mut self, repr: NodeRepr) -> usize {
        let slots = repr
            .slots
            .into_iter()
            .map(|s| match s {
                SlotRepr::Entry { key, value } => MemSlot::Entry { key, value },
                SlotRepr::Child { hash } => MemSlot::Stored(hash),
            })
            .collect();
        self.nodes.push(MemNode {
            bitmap: repr.bitmap,
            slots,
        });
        self.nodes.len() - 1
    }

    fn classify(&self, node: usize, pos: usize, key: &str) -> SlotCase {
        match &self.nodes[node].slots[pos] {
            MemSlot::Entry { key: k, .. } if k == key => SlotCase::SameKey,
            MemSlot::Entry { .. } => SlotCase::OtherKey,
            MemSlot::Stored(h) => SlotCase::Load(*h),
            MemSlot::Mem(idx) => SlotCase::Descend(*idx),
        }
    }

    /// Insert or replace a key.
    pub(crate) async fn insert<S: Storage>(
        &mut self,
        storage: &S,
        key: &str,
        value: Value,
    ) -> Result<()> {
        let kh = key_hash(key);
        let mut cur = self.root;
        let mut depth = 0;
        loop {
            if depth > MAX_DEPTH {
                return Err(depth_exceeded());
            }
            let bit = 1u32 << chunk(&kh, depth);
            let pos = slot_pos(self.nodes[cur].bitmap, bit);

            if self.nodes[cur].bitmap & bit == 0 {
                self.nodes[cur].slots.insert(
                    pos,
                    MemSlot::Entry {
                        key: key.to_string(),
                        value,
                    },
                );
                self.nodes[cur].bitmap |= bit;
                return Ok(());
            }

            match self.classify(cur, pos, key) {
                SlotCase::SameKey => {
                    if let MemSlot::Entry { value: v, .. } = &mut self.nodes[cur].slots[pos] {
                        *v = value;
                    }
                    return Ok(());
                }
                SlotCase::OtherKey => {
                    // Two keys collide at this chunk: push the existing
                    // entry down through a chain of single-child nodes
                    // until the two keys' chunks diverge, then store both.
                    let old = mem::replace(&mut self.nodes[cur].slots[pos], MemSlot::Mem(0));
                    let (okey, ovalue) = match old {
                        MemSlot::Entry { key, value } => (key, value),
                        _ => return Err(Error::Corrupt("slot changed during insert".to_string())),
                    };
                    let okh = key_hash(&okey);
                    let mut chain = self.push_empty();
                    self.nodes[cur].slots[pos] = MemSlot::Mem(chain);
                    let mut d = depth + 1;
                    loop {
                        if d > MAX_DEPTH {
                            return Err(depth_exceeded());
                        }
                        let oc = chunk(&okh, d);
                        let nc = chunk(&kh, d);
                        if oc == nc {
                            let next = self.push_empty();
                            self.nodes[chain].bitmap = 1u32 << oc;
                            self.nodes[chain].slots = vec![MemSlot::Mem(next)];
                            chain = next;
                            d += 1;
                            continue;
                        }
                        let old_slot = MemSlot::Entry {
                            key: okey,
                            value: ovalue,
                        };
                        let new_slot = MemSlot::Entry {
                            key: key.to_string(),
                            value,
                        };
                        self.nodes[chain].bitmap = (1u32 << oc) | (1u32 << nc);
                        self.nodes[chain].slots = if oc < nc {
                            vec![old_slot, new_slot]
                        } else {
                            vec![new_slot, old_slot]
                        };
                        return Ok(());
                    }
                }
                SlotCase::Load(h) => {
                    let repr = load_node(storage, &h).await?;
                    let idx = self.push_from_repr(repr);
                    self.nodes[cur].slots[pos] = MemSlot::Mem(idx);
                    cur = idx;
                    depth += 1;
                }
                SlotCase::Descend(idx) => {
                    cur = idx;
                    depth += 1;
                }
            }
        }
    }

    /// Remove a key. Returns `true` if it was present.
    pub(crate) async fn remove<S: Storage>(&mut self, storage: &S, key: &str) -> Result<bool> {
        let kh = key_hash(key);
        let mut cur = self.root;
        let mut depth = 0;
        // (node, slot position descended through) for collapse on unwind.
        let mut path: Vec<(usize, usize)> = Vec::new();
        loop {
            if depth > MAX_DEPTH {
                return Err(depth_exceeded());
            }
            let bit = 1u32 << chunk(&kh, depth);
            if self.nodes[cur].bitmap & bit == 0 {
                return Ok(false);
            }
            let pos = slot_pos(self.nodes[cur].bitmap, bit);

            match self.classify(cur, pos, key) {
                SlotCase::SameKey => {
                    self.nodes[cur].slots.remove(pos);
                    self.nodes[cur].bitmap &= !bit;
                    self.collapse(path);
                    return Ok(true);
                }
                SlotCase::OtherKey => return Ok(false),
                SlotCase::Load(h) => {
                    let repr = load_node(storage, &h).await?;
                    let idx = self.push_from_repr(repr);
                    self.nodes[cur].slots[pos] = MemSlot::Mem(idx);
                    path.push((cur, pos));
                    cur = idx;
                    depth += 1;
                }
                SlotCase::Descend(idx) => {
                    path.push((cur, pos));
                    cur = idx;
                    depth += 1;
                }
            }
        }
    }

    /// Canonical-form maintenance after a removal: walking the descent path
    /// upward, any node left with exactly one slot that is an *entry* is
    /// inlined into its parent. Stops at the first node that keeps more
    /// than an entry; the root is never collapsed. Orphaned arena nodes are
    /// simply never flushed.
    fn collapse(&mut self, path: Vec<(usize, usize)>) {
        for (parent, pos) in path.into_iter().rev() {
            let child = match self.nodes[parent].slots[pos] {
                MemSlot::Mem(idx) => idx,
                _ => return,
            };
            let single_entry = self.nodes[child].slots.len() == 1
                && matches!(self.nodes[child].slots[0], MemSlot::Entry { .. });
            if !single_entry {
                return;
            }
            if let Some(entry) = self.nodes[child].slots.pop() {
                self.nodes[child].bitmap = 0;
                self.nodes[parent].slots[pos] = entry;
            }
        }
    }

    /// Write every live arena node (post-order) and return the new root
    /// hash.
    pub(crate) async fn flush<S: Storage>(mut self, storage: &S) -> Result<Hash> {
        enum Task {
            Enter(usize),
            Exit(usize),
        }
        let mut hashes: Vec<Option<Hash>> = vec![None; self.nodes.len()];
        let mut stack = vec![Task::Enter(self.root)];
        while let Some(task) = stack.pop() {
            match task {
                Task::Enter(i) => {
                    stack.push(Task::Exit(i));
                    for slot in &self.nodes[i].slots {
                        if let MemSlot::Mem(child) = slot {
                            stack.push(Task::Enter(*child));
                        }
                    }
                }
                Task::Exit(i) => {
                    let bitmap = self.nodes[i].bitmap;
                    let mut slots = Vec::with_capacity(self.nodes[i].slots.len());
                    for slot in mem::take(&mut self.nodes[i].slots) {
                        slots.push(match slot {
                            MemSlot::Entry { key, value } => SlotRepr::Entry { key, value },
                            MemSlot::Stored(hash) => SlotRepr::Child { hash },
                            MemSlot::Mem(child) => SlotRepr::Child {
                                hash: hashes[child].ok_or_else(|| {
                                    Error::Corrupt("flush visited child out of order".to_string())
                                })?,
                            },
                        });
                    }
                    let hash = put_object(
                        storage,
                        &NodeRepr {
                            tag: TAG_NODE.to_string(),
                            bitmap,
                            slots,
                        },
                    )
                    .await?;
                    hashes[i] = Some(hash);
                }
            }
        }
        hashes[self.root].ok_or_else(|| Error::Corrupt("flush produced no root".to_string()))
    }
}
