//! Structural diff between two state trees.
//!
//! Because nodes are content-addressed, any subtree with the same hash on
//! both sides is provably identical and is skipped without being loaded.
//! Work is proportional to the amount of change, not the size of the state.

use std::collections::BTreeMap;

use crate::tree::{collect_entries, load_node_slots, NodeSlot};
use crate::{Hash, Result, Storage, Value};

/// How a single key changed between two commits.
#[derive(Debug, Clone, PartialEq)]
pub enum Change {
    /// The key exists only in the newer tree.
    Added {
        /// The new value.
        new: Value,
    },
    /// The key exists only in the older tree.
    Removed {
        /// The removed value.
        old: Value,
    },
    /// The key exists in both trees with different values.
    Modified {
        /// The value in the older tree.
        old: Value,
        /// The value in the newer tree.
        new: Value,
    },
}

/// One changed key in a diff.
#[derive(Debug, Clone, PartialEq)]
pub struct DiffEntry {
    /// The state key that changed.
    pub key: String,
    /// What happened to it.
    pub change: Change,
}

/// Diff the trees rooted at `a` (old) and `b` (new). Entries are returned
/// sorted by key.
pub(crate) async fn tree_diff<S: Storage>(
    storage: &S,
    a: &Hash,
    b: &Hash,
) -> Result<Vec<DiffEntry>> {
    let mut out = Vec::new();
    // Pairs of (old subtree, new subtree) still to compare; iterative so no
    // boxed recursive futures are needed.
    let mut pairs: Vec<(Hash, Hash)> = Vec::new();
    if a != b {
        pairs.push((*a, *b));
    }
    while let Some((a, b)) = pairs.pop() {
        let (a_bitmap, a_slots) = load_node_slots(storage, &a).await?;
        let (b_bitmap, b_slots) = load_node_slots(storage, &b).await?;
        let mut a_iter = a_slots.into_iter();
        let mut b_iter = b_slots.into_iter();

        for idx in 0..32u32 {
            let bit = 1u32 << idx;
            let sa = if a_bitmap & bit != 0 {
                a_iter.next()
            } else {
                None
            };
            let sb = if b_bitmap & bit != 0 {
                b_iter.next()
            } else {
                None
            };
            match (sa, sb) {
                (None, None) => {}
                (Some(NodeSlot::Child(ha)), Some(NodeSlot::Child(hb))) => {
                    if ha != hb {
                        pairs.push((ha, hb));
                    }
                }
                (sa, sb) => {
                    // Entry vs entry, entry vs subtree, or one side absent:
                    // materialize both sides fully and set-diff them. A
                    // subtree here holds only genuinely differing content.
                    let mut old = BTreeMap::new();
                    let mut new = BTreeMap::new();
                    materialize(storage, sa, &mut old).await?;
                    materialize(storage, sb, &mut new).await?;
                    diff_maps(old, new, &mut out);
                }
            }
        }
    }
    out.sort_by(|x, y| x.key.cmp(&y.key));
    Ok(out)
}

/// Expand one side of a slot into its full key→value map.
async fn materialize<S: Storage>(
    storage: &S,
    side: Option<NodeSlot>,
    out: &mut BTreeMap<String, Value>,
) -> Result<()> {
    match side {
        None => Ok(()),
        Some(NodeSlot::Entry(key, value)) => {
            out.insert(key, value);
            Ok(())
        }
        Some(NodeSlot::Child(hash)) => collect_entries(storage, &hash, out).await,
    }
}

/// Set-diff two fully materialized maps.
fn diff_maps(
    old: BTreeMap<String, Value>,
    mut new: BTreeMap<String, Value>,
    out: &mut Vec<DiffEntry>,
) {
    for (key, old_value) in old {
        match new.remove(&key) {
            None => out.push(DiffEntry {
                key,
                change: Change::Removed { old: old_value },
            }),
            Some(new_value) => {
                if new_value != old_value {
                    out.push(DiffEntry {
                        key,
                        change: Change::Modified {
                            old: old_value,
                            new: new_value,
                        },
                    });
                }
            }
        }
    }
    for (key, new_value) in new {
        out.push(DiffEntry {
            key,
            change: Change::Added { new: new_value },
        });
    }
}
