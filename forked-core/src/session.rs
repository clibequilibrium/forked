//! The engine API: sessions, branches, commits, fork, checkout, log, diff
//! and the effect log, all generic over a [`Storage`] backend.

use std::collections::{BTreeMap, HashSet};

use crate::commit::{Commit, CommitRepr, LogEntry, Meta};
use crate::diff::DiffEntry;
use crate::encoding::{get_object, put_object};
use crate::tree::{collect_entries, put_empty_root, tree_get, MemTree};
use crate::{diff, EffectRecord, Error, Hash, Result, Storage, Value};

/// The branch every new session starts with.
pub const DEFAULT_BRANCH: &str = "main";

/// A single state change inside a commit.
#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    /// Set `key` to `value` (insert or replace).
    Put(String, Value),
    /// Remove `key` if present.
    Delete(String),
}

/// Everything that goes into one commit. All fields default to empty, so
/// `CommitInput { ops, ..Default::default() }` is the common case.
#[derive(Debug, Clone, Default)]
pub struct CommitInput {
    /// State changes, applied in order.
    pub ops: Vec<Op>,
    /// Caller metadata (message, actor, timestamp, …).
    pub meta: Meta,
    /// Side effects recorded atomically with this commit. For idempotency
    /// checking, prefer [`Forked::record_effect`].
    pub effects: Vec<EffectRecord>,
}

/// Outcome of [`Forked::record_effect`].
#[derive(Debug, Clone, PartialEq)]
pub enum EffectOutcome {
    /// The effect was new; it was recorded in a fresh commit.
    Recorded {
        /// The commit that now carries the record.
        commit: Hash,
        /// The record as stored.
        record: EffectRecord,
    },
    /// An effect with this idempotency key already exists somewhere in the
    /// session (any branch, including rewound-away history). The stored
    /// record, result included, is returned instead of re-running.
    AlreadyRecorded(EffectRecord),
}

/// The forked engine over a storage backend.
///
/// Cheap to construct; holds no state beyond the backend handle. All
/// methods are `async` and safe to call from `!Send` contexts (wasm).
pub struct Forked<S> {
    storage: S,
}

impl<S: Storage> Forked<S> {
    /// Wrap a storage backend.
    pub fn new(storage: S) -> Self {
        Forked { storage }
    }

    /// Access the underlying storage.
    pub fn storage(&self) -> &S {
        &self.storage
    }

    // -- sessions and branches ------------------------------------------------

    /// Create a session: an empty state tree, a root commit, and a `main`
    /// branch pointing at it. Returns the root commit hash.
    pub async fn create_session(&self, session: &str) -> Result<Hash> {
        validate_name(session)?;
        let root = put_empty_root(&self.storage).await?;
        let hash = self
            .write_commit(Commit {
                parent: None,
                root,
                meta: Meta::new(),
                effects: Vec::new(),
            })
            .await?;
        let created = self
            .storage
            .cas_ref(&branch_ref(session, DEFAULT_BRANCH), None, &hash)
            .await?;
        if !created {
            return Err(Error::SessionExists(session.to_string()));
        }
        Ok(hash)
    }

    /// Current head commit of a branch.
    pub async fn head(&self, session: &str, branch: &str) -> Result<Hash> {
        validate_name(session)?;
        validate_name(branch)?;
        self.storage
            .get_ref(&branch_ref(session, branch))
            .await?
            .ok_or_else(|| Error::NotFound(format!("branch {session}/{branch}")))
    }

    /// List the session's branches with their heads, sorted by name.
    pub async fn branches(&self, session: &str) -> Result<Vec<(String, Hash)>> {
        validate_name(session)?;
        let prefix = format!("s/{session}/b/");
        let mut refs = self.storage.list_refs(&prefix).await?;
        refs.sort();
        Ok(refs
            .into_iter()
            .map(|(name, hash)| (name[prefix.len()..].to_string(), hash))
            .collect())
    }

    /// Fork: create branch `new` pointing at the head of `from`. O(1), a
    /// single ref is written; no state is copied.
    pub async fn fork(&self, session: &str, from: &str, new: &str) -> Result<Hash> {
        validate_name(new)?;
        let head = self.head(session, from).await?;
        let created = self
            .storage
            .cas_ref(&branch_ref(session, new), None, &head)
            .await?;
        if !created {
            return Err(Error::BranchExists(new.to_string()));
        }
        Ok(head)
    }

    /// Rewind (checkout): move a branch head to another commit of the same
    /// session, typically an ancestor.
    ///
    /// The abandoned head is preserved under an *anchor* ref before the
    /// branch moves, so the commits past the rewind point, and the effect
    /// records inside them, stay reachable forever. Effects are never
    /// undone by rewind.
    pub async fn checkout(&self, session: &str, branch: &str, target: &Hash) -> Result<()> {
        let head = self.head(session, branch).await?;
        if head == *target {
            return Ok(());
        }
        if !self.commit_in_session(session, target).await? {
            return Err(Error::NotFound(format!(
                "commit {target} in session {session}"
            )));
        }
        // Anchor the current head first; if we crash between the two writes
        // the stray anchor is harmless. The anchor CAS failing just means
        // the anchor already exists.
        let _ = self
            .storage
            .cas_ref(&anchor_ref(session, &head), None, &head)
            .await?;
        let moved = self
            .storage
            .cas_ref(&branch_ref(session, branch), Some(&head), target)
            .await?;
        if !moved {
            return Err(Error::Conflict(branch.to_string()));
        }
        Ok(())
    }

    // -- state ---------------------------------------------------------------

    /// Read one key from a branch's current state.
    pub async fn get(&self, session: &str, branch: &str, key: &str) -> Result<Option<Value>> {
        let commit = self.read_commit(&self.head(session, branch).await?).await?;
        tree_get(&self.storage, &commit.root, key).await
    }

    /// Materialize a branch's entire current state.
    pub async fn snapshot(&self, session: &str, branch: &str) -> Result<BTreeMap<String, Value>> {
        let commit = self.read_commit(&self.head(session, branch).await?).await?;
        self.snapshot_at(&commit.root).await
    }

    /// Materialize the full state under a tree root hash.
    pub async fn snapshot_at(&self, root: &Hash) -> Result<BTreeMap<String, Value>> {
        let mut out = BTreeMap::new();
        collect_entries(&self.storage, root, &mut out).await?;
        Ok(out)
    }

    // -- commits ---------------------------------------------------------------

    /// Commit on top of the branch's current head. Equivalent to reading
    /// [`head`](Self::head) and calling [`commit_at`](Self::commit_at).
    pub async fn commit(&self, session: &str, branch: &str, input: CommitInput) -> Result<Hash> {
        let head = self.head(session, branch).await?;
        self.commit_at(session, branch, &head, input).await
    }

    /// Commit on top of a specific expected head (optimistic concurrency).
    ///
    /// If the branch has moved since `expected_head` was read, nothing is
    /// committed and [`Error::Conflict`] is returned; the caller re-reads
    /// the head and retries. The ref CAS is the only point of coordination,
    /// which is what makes "commit + record effects" atomic everywhere.
    pub async fn commit_at(
        &self,
        session: &str,
        branch: &str,
        expected_head: &Hash,
        input: CommitInput,
    ) -> Result<Hash> {
        validate_name(session)?;
        validate_name(branch)?;
        let parent = self.read_commit(expected_head).await?;
        let root = if input.ops.is_empty() {
            parent.root
        } else {
            let mut tree = MemTree::load(&self.storage, &parent.root).await?;
            for op in input.ops {
                match op {
                    Op::Put(key, value) => tree.insert(&self.storage, &key, value).await?,
                    Op::Delete(key) => {
                        tree.remove(&self.storage, &key).await?;
                    }
                }
            }
            tree.flush(&self.storage).await?
        };
        let hash = self
            .write_commit(Commit {
                parent: Some(*expected_head),
                root,
                meta: input.meta,
                effects: input.effects,
            })
            .await?;
        let moved = self
            .storage
            .cas_ref(&branch_ref(session, branch), Some(expected_head), &hash)
            .await?;
        if !moved {
            // The new blobs are orphaned, which is harmless; GC can sweep
            // them later.
            return Err(Error::Conflict(branch.to_string()));
        }
        Ok(hash)
    }

    /// Walk a branch's history from its head, newest first. `limit` bounds
    /// the number of entries returned; `None` walks to the root commit.
    pub async fn log(
        &self,
        session: &str,
        branch: &str,
        limit: Option<usize>,
    ) -> Result<Vec<LogEntry>> {
        let mut cursor = Some(self.head(session, branch).await?);
        let mut out = Vec::new();
        while let Some(hash) = cursor {
            if limit.is_some_and(|l| out.len() >= l) {
                break;
            }
            let commit = self.read_commit(&hash).await?;
            cursor = commit.parent;
            out.push(LogEntry { hash, commit });
        }
        Ok(out)
    }

    /// Diff the states of two commits (old → new), sorted by key. Shared
    /// subtrees are skipped by hash without being loaded.
    pub async fn diff(&self, a: &Hash, b: &Hash) -> Result<Vec<DiffEntry>> {
        let ca = self.read_commit(a).await?;
        let cb = self.read_commit(b).await?;
        diff::tree_diff(&self.storage, &ca.root, &cb.root).await
    }

    /// Decode the commit object at `hash`.
    pub async fn read_commit(&self, hash: &Hash) -> Result<Commit> {
        let repr: CommitRepr = get_object(&self.storage, hash).await?;
        Commit::from_repr(repr)
    }

    // -- effects ----------------------------------------------------------------

    /// Record a side effect idempotently.
    ///
    /// If `effect.key` already exists anywhere in the session, any branch,
    /// including history abandoned by rewind, the stored record is
    /// returned and nothing is written. Otherwise the record is committed
    /// on `branch` (state unchanged) and returned as `Recorded`.
    ///
    /// The existence check and the commit are not one atomic step across
    /// *concurrent* writers; serialize writers per session (as a Durable
    /// Object does) for a strict exactly-once guarantee.
    pub async fn record_effect(
        &self,
        session: &str,
        branch: &str,
        effect: EffectRecord,
    ) -> Result<EffectOutcome> {
        if let Some(existing) = self.find_effect(session, &effect.key).await? {
            return Ok(EffectOutcome::AlreadyRecorded(existing));
        }
        let head = self.head(session, branch).await?;
        let parent = self.read_commit(&head).await?;
        let hash = self
            .write_commit(Commit {
                parent: Some(head),
                root: parent.root,
                meta: Meta::new(),
                effects: vec![effect.clone()],
            })
            .await?;
        let moved = self
            .storage
            .cas_ref(&branch_ref(session, branch), Some(&head), &hash)
            .await?;
        if !moved {
            return Err(Error::Conflict(branch.to_string()));
        }
        Ok(EffectOutcome::Recorded {
            commit: hash,
            record: effect,
        })
    }

    /// Find an effect record by idempotency key, searching every commit
    /// reachable from any of the session's branches and anchors.
    pub async fn find_effect(&self, session: &str, key: &str) -> Result<Option<EffectRecord>> {
        validate_name(session)?;
        let mut found: Option<EffectRecord> = None;
        self.walk_session(session, |_, commit| {
            if found.is_none() {
                if let Some(e) = commit.effects.iter().find(|e| e.key == key) {
                    found = Some(e.clone());
                }
            }
        })
        .await?;
        Ok(found)
    }

    // -- graph -------------------------------------------------------------------

    /// The whole session as a graph: every branch, every anchor, and every
    /// reachable commit. Commits are returned sorted by hash (clients
    /// topo-sort via parent pointers); this powers visual branch trees.
    pub async fn graph(&self, session: &str) -> Result<SessionGraph> {
        validate_name(session)?;
        let branches = self.branches(session).await?;
        if branches.is_empty() {
            return Err(Error::NotFound(format!("session {session}")));
        }
        let anchor_prefix = format!("s/{session}/a/");
        let mut anchors: Vec<Hash> = self
            .storage
            .list_refs(&anchor_prefix)
            .await?
            .into_iter()
            .map(|(_, h)| h)
            .collect();
        anchors.sort();
        let mut commits = Vec::new();
        self.walk_session(session, |hash, commit| {
            commits.push(LogEntry {
                hash,
                commit: commit.clone(),
            });
        })
        .await?;
        commits.sort_by_key(|e| e.hash);
        Ok(SessionGraph {
            branches,
            anchors,
            commits,
        })
    }

    // -- internals ---------------------------------------------------------------

    async fn write_commit(&self, commit: Commit) -> Result<Hash> {
        put_object(&self.storage, &commit.into_repr()).await
    }

    /// Is `target` a commit reachable from this session's refs?
    async fn commit_in_session(&self, session: &str, target: &Hash) -> Result<bool> {
        let mut hit = false;
        self.walk_session(session, |hash, _| {
            if hash == *target {
                hit = true;
            }
        })
        .await?;
        Ok(hit)
    }

    /// Visit every commit reachable from the session's branch and anchor
    /// refs, each exactly once.
    async fn walk_session(
        &self,
        session: &str,
        mut visit: impl FnMut(Hash, &Commit),
    ) -> Result<()> {
        let refs = self.storage.list_refs(&format!("s/{session}/")).await?;
        let mut stack: Vec<Hash> = refs.into_iter().map(|(_, h)| h).collect();
        let mut seen: HashSet<Hash> = HashSet::new();
        while let Some(hash) = stack.pop() {
            if !seen.insert(hash) {
                continue;
            }
            let commit = self.read_commit(&hash).await?;
            visit(hash, &commit);
            if let Some(parent) = commit.parent {
                stack.push(parent);
            }
        }
        Ok(())
    }
}

/// Everything reachable in one session: branches, anchors, and commits.
/// Returned by [`Forked::graph`].
#[derive(Debug, Clone, PartialEq)]
pub struct SessionGraph {
    /// Branch names with their head commits, sorted by name.
    pub branches: Vec<(String, Hash)>,
    /// Heads preserved by rewinds, sorted.
    pub anchors: Vec<Hash>,
    /// Every commit reachable from any branch or anchor, sorted by hash.
    pub commits: Vec<LogEntry>,
}

/// Ref name for a branch head.
fn branch_ref(session: &str, branch: &str) -> String {
    format!("s/{session}/b/{branch}")
}

/// Ref name for an anchor (a head preserved across rewind). Keyed by the
/// anchored commit's hash, so anchoring is idempotent.
fn anchor_ref(session: &str, head: &Hash) -> String {
    format!("s/{session}/a/{head}")
}

/// Session and branch names: 1–128 chars of `[A-Za-z0-9._-]`. Keeps ref
/// names unambiguous (no `/`) across every backend.
fn validate_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 128
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-');
    if ok {
        Ok(())
    } else {
        Err(Error::InvalidName(name.to_string()))
    }
}
