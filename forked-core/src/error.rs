//! Error types. Core never panics in normal paths; everything surfaces as
//! [`Error`], including backend failures via [`StorageError`].

use crate::Hash;

/// An error reported by a storage adapter.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct StorageError {
    /// Human-readable description of the backend failure.
    pub message: String,
}

impl StorageError {
    /// Build a storage error from any message.
    pub fn new(message: impl Into<String>) -> Self {
        StorageError {
            message: message.into(),
        }
    }
}

/// Any error produced by forked-core.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The storage backend failed.
    #[error("storage error: {0}")]
    Storage(#[from] StorageError),

    /// A compare-and-swap on a branch ref lost a race: the branch moved
    /// since the caller read its head. Re-read the head and retry.
    #[error("conflict: branch {0} was modified concurrently")]
    Conflict(String),

    /// A session, branch or commit does not exist.
    #[error("not found: {0}")]
    NotFound(String),

    /// Attempted to create a branch that already exists.
    #[error("branch already exists: {0}")]
    BranchExists(String),

    /// Attempted to create a session that already exists.
    #[error("session already exists: {0}")]
    SessionExists(String),

    /// A blob referenced by a commit or node is absent from storage.
    #[error("missing blob: {0}")]
    MissingBlob(Hash),

    /// Stored bytes failed to decode or violate a format invariant.
    #[error("corrupt data: {0}")]
    Corrupt(String),

    /// A value failed to encode (should not happen for well-formed input).
    #[error("encoding error: {0}")]
    Encoding(String),

    /// A session, branch or key name is not allowed.
    #[error("invalid name: {0}")]
    InvalidName(String),

    /// A hex string is not a valid hash.
    #[error("invalid hash: {0}")]
    InvalidHash(String),
}

/// Result alias used throughout forked-core.
pub type Result<T, E = Error> = core::result::Result<T, E>;
