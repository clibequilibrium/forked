//! JSON request/response types for the forked HTTP API (v1).
//!
//! Hashes appear as 64-char lowercase hex strings on the wire. These types
//! are the single source of truth for both the self-hosted server and
//! forked cloud; the TypeScript SDK mirrors them.

use std::collections::BTreeMap;

use forked_core::{Change, Commit, DiffEntry, EffectRecord, Hash, LogEntry, Value};
use serde::{Deserialize, Serialize};

/// Commit metadata on the wire.
pub type MetaDto = BTreeMap<String, Value>;

/// `POST /v1/sessions` request body. `session_id` is optional; the server
/// generates one when absent.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct CreateSessionRequest {
    /// Optional caller-chosen session id (`[A-Za-z0-9._-]{1,128}`).
    #[serde(default)]
    pub session_id: Option<String>,
}

/// `POST /v1/sessions` response.
#[derive(Debug, Serialize, Deserialize)]
pub struct CreateSessionResponse {
    /// The session's id.
    pub session_id: String,
    /// The initial branch (always `main`).
    pub branch: String,
    /// Hash of the session's root commit.
    pub head: String,
}

/// One state operation inside a commit, tagged by `"op"`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum OpDto {
    /// Set `key` to `value`.
    Put {
        /// The state key.
        key: String,
        /// The new value.
        value: Value,
    },
    /// Remove `key`.
    Delete {
        /// The state key.
        key: String,
    },
}

/// A side-effect record on the wire.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EffectDto {
    /// Idempotency key, unique per session.
    pub key: String,
    /// Effect kind (free-form, e.g. `"email.send"`).
    pub kind: String,
    /// The input that produced the effect.
    #[serde(default)]
    pub payload: Value,
    /// The observed result.
    #[serde(default)]
    pub result: Value,
}

/// `POST …/commit` request body.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct CommitRequest {
    /// State changes, applied in order.
    #[serde(default)]
    pub ops: Vec<OpDto>,
    /// Commit metadata.
    #[serde(default)]
    pub meta: MetaDto,
    /// Effects recorded atomically with the commit (no idempotency check;
    /// use `POST …/effects` for that).
    #[serde(default)]
    pub effects: Vec<EffectDto>,
    /// Optimistic concurrency: commit only if the branch head still equals
    /// this hash. Omitted = commit on the current head.
    #[serde(default)]
    pub expected_head: Option<String>,
}

/// Response carrying the new head after a commit.
#[derive(Debug, Serialize, Deserialize)]
pub struct HeadResponse {
    /// The branch's new head commit hash.
    pub head: String,
}

/// `GET …/state` response.
#[derive(Debug, Serialize, Deserialize)]
pub struct StateResponse {
    /// The head commit the state was read from.
    pub head: String,
    /// The full key→value state.
    pub state: BTreeMap<String, Value>,
}

/// `POST …/fork` request body.
#[derive(Debug, Serialize, Deserialize)]
pub struct ForkRequest {
    /// Name for the new branch.
    pub branch: String,
}

/// `POST …/checkout` request body.
#[derive(Debug, Serialize, Deserialize)]
pub struct CheckoutRequest {
    /// Commit hash to move the branch head to.
    pub commit: String,
}

/// A branch name + head pair.
#[derive(Debug, Serialize, Deserialize)]
pub struct BranchDto {
    /// Branch name.
    pub name: String,
    /// Head commit hash.
    pub head: String,
}

/// Response naming a branch and its head.
#[derive(Debug, Serialize, Deserialize)]
pub struct BranchResponse {
    /// The branch acted on.
    pub branch: String,
    /// Its (new) head.
    pub head: String,
}

/// `GET …/branches` response.
#[derive(Debug, Serialize, Deserialize)]
pub struct BranchesResponse {
    /// All branches, sorted by name.
    pub branches: Vec<BranchDto>,
}

/// A commit on the wire.
#[derive(Debug, Serialize, Deserialize)]
pub struct CommitDto {
    /// This commit's hash.
    pub hash: String,
    /// Parent commit hash; null for the session root.
    pub parent: Option<String>,
    /// Root hash of the state tree.
    pub root: String,
    /// Commit metadata.
    pub meta: MetaDto,
    /// Effects recorded in this commit.
    pub effects: Vec<EffectDto>,
}

/// `GET …/log` response (newest first).
#[derive(Debug, Serialize, Deserialize)]
pub struct LogResponse {
    /// Commits from the branch head backwards.
    pub entries: Vec<CommitDto>,
}

/// One changed key in a diff.
#[derive(Debug, Serialize, Deserialize)]
pub struct DiffEntryDto {
    /// The state key.
    pub key: String,
    /// `"added"`, `"removed"` or `"modified"`.
    pub change: String,
    /// Old value (absent for `added`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old: Option<Value>,
    /// New value (absent for `removed`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new: Option<Value>,
}

/// `GET …/diff?a=…&b=…` response.
#[derive(Debug, Serialize, Deserialize)]
pub struct DiffResponse {
    /// Changed keys, sorted.
    pub entries: Vec<DiffEntryDto>,
}

/// `GET …/graph` response: the whole session for visualization.
#[derive(Debug, Serialize, Deserialize)]
pub struct GraphResponse {
    /// All branches with heads.
    pub branches: Vec<BranchDto>,
    /// Heads preserved by rewinds.
    pub anchors: Vec<String>,
    /// Every reachable commit (topo-sort client-side via `parent`).
    pub commits: Vec<CommitDto>,
}

/// `POST …/effects` request body.
#[derive(Debug, Serialize, Deserialize)]
pub struct RecordEffectRequest {
    /// Branch to record on (default `main`). Lookup is session-wide
    /// regardless.
    #[serde(default)]
    pub branch: Option<String>,
    /// Idempotency key.
    pub key: String,
    /// Effect kind.
    pub kind: String,
    /// Effect input.
    #[serde(default)]
    pub payload: Value,
    /// Observed result.
    #[serde(default)]
    pub result: Value,
}

/// `POST …/effects` response.
#[derive(Debug, Serialize, Deserialize)]
pub struct RecordEffectResponse {
    /// `"recorded"` (new) or `"already_recorded"` (idempotent replay).
    pub status: String,
    /// The stored record; on replay, the original one with its result.
    pub record: EffectDto,
    /// Commit that carries the record (absent on replay).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
}

/// `GET …/effects?key=…` response: the stored record, or null if the key
/// has never been recorded in this session.
#[derive(Debug, Serialize, Deserialize)]
pub struct FindEffectResponse {
    /// The stored record, if any.
    pub record: Option<EffectDto>,
}

/// `GET /v1/health` response.
#[derive(Debug, Serialize, Deserialize)]
pub struct HealthResponse {
    /// Always `"ok"`.
    pub status: String,
}

// -- conversions --------------------------------------------------------------

impl From<EffectRecord> for EffectDto {
    fn from(e: EffectRecord) -> Self {
        EffectDto {
            key: e.key,
            kind: e.kind,
            payload: e.payload,
            result: e.result,
        }
    }
}

impl From<EffectDto> for EffectRecord {
    fn from(e: EffectDto) -> Self {
        EffectRecord {
            key: e.key,
            kind: e.kind,
            payload: e.payload,
            result: e.result,
        }
    }
}

/// Convert a core log entry into its wire form.
pub fn commit_dto(entry: LogEntry) -> CommitDto {
    let LogEntry { hash, commit } = entry;
    let Commit {
        parent,
        root,
        meta,
        effects,
    } = commit;
    CommitDto {
        hash: hash.to_hex(),
        parent: parent.map(|h| h.to_hex()),
        root: root.to_hex(),
        meta,
        effects: effects.into_iter().map(EffectDto::from).collect(),
    }
}

/// Convert a core diff entry into its wire form.
pub fn diff_entry_dto(entry: DiffEntry) -> DiffEntryDto {
    let (change, old, new) = match entry.change {
        Change::Added { new } => ("added", None, Some(new)),
        Change::Removed { old } => ("removed", Some(old), None),
        Change::Modified { old, new } => ("modified", Some(old), Some(new)),
    };
    DiffEntryDto {
        key: entry.key,
        change: change.to_string(),
        old,
        new,
    }
}

/// Convert a `(name, head)` pair into its wire form.
pub fn branch_dto(pair: (String, Hash)) -> BranchDto {
    BranchDto {
        name: pair.0,
        head: pair.1.to_hex(),
    }
}
