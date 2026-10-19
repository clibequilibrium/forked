//! Commit and effect-record objects.
//!
//! Effect records live *inside* the commit object. That is a deliberate
//! design decision: the single compare-and-swap that advances a branch ref
//! makes "commit state + record effects" atomic on every backend, including
//! plain object storage.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{Error, Hash, Result, Value};

/// Commit metadata: arbitrary JSON values keyed by string (sorted map, so
/// encoding stays deterministic).
pub type Meta = BTreeMap<String, Value>;

/// A record of a side effect performed by the agent (an email sent, a
/// payment made, an API call issued).
///
/// Effects are never undone by rewind. Recording an effect whose
/// idempotency key already exists anywhere in the session returns the
/// stored result instead of re-running the effect.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EffectRecord {
    /// Idempotency key, unique per session (e.g. `"send-email:42"`).
    #[serde(rename = "k")]
    pub key: String,
    /// What kind of effect this was (free-form, e.g. `"email.send"`).
    #[serde(rename = "d")]
    pub kind: String,
    /// The input that produced the effect, for audit/replay.
    #[serde(rename = "p")]
    pub payload: Value,
    /// The observed result, returned verbatim on idempotent replays.
    #[serde(rename = "r")]
    pub result: Value,
}

/// A commit: an immutable point in a session's history.
#[derive(Debug, Clone, PartialEq)]
pub struct Commit {
    /// The previous commit on this line of history; `None` for the root
    /// commit created with the session.
    pub parent: Option<Hash>,
    /// Content hash of the root node of the state tree.
    pub root: Hash,
    /// Caller-supplied metadata (message, actor, timestamps, …).
    pub meta: Meta,
    /// Side effects recorded atomically with this commit.
    pub effects: Vec<EffectRecord>,
}

/// On-disk form of [`Commit`]; carries the object-kind tag.
#[derive(Serialize, Deserialize)]
pub(crate) struct CommitRepr {
    #[serde(rename = "t")]
    pub tag: String,
    #[serde(rename = "p")]
    pub parent: Option<Hash>,
    #[serde(rename = "r")]
    pub root: Hash,
    #[serde(rename = "m")]
    pub meta: Meta,
    #[serde(rename = "e")]
    pub effects: Vec<EffectRecord>,
}

/// Object-kind tag for commits.
pub(crate) const TAG_COMMIT: &str = "c";

impl Commit {
    pub(crate) fn into_repr(self) -> CommitRepr {
        CommitRepr {
            tag: TAG_COMMIT.to_string(),
            parent: self.parent,
            root: self.root,
            meta: self.meta,
            effects: self.effects,
        }
    }

    pub(crate) fn from_repr(repr: CommitRepr) -> Result<Self> {
        if repr.tag != TAG_COMMIT {
            return Err(Error::Corrupt(format!(
                "expected commit object, found tag {:?}",
                repr.tag
            )));
        }
        Ok(Commit {
            parent: repr.parent,
            root: repr.root,
            meta: repr.meta,
            effects: repr.effects,
        })
    }
}

/// A commit hash paired with its decoded commit, as returned by `log`.
#[derive(Debug, Clone, PartialEq)]
pub struct LogEntry {
    /// Content hash of the commit object.
    pub hash: Hash,
    /// The decoded commit.
    pub commit: Commit,
}
