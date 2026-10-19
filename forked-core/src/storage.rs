//! The storage abstraction. Two fundamental operations back everything:
//!
//! 1. put/get of immutable blobs by content hash (idempotent), and
//! 2. compare-and-swap on a small named ref (move a branch from commit A to
//!    commit B only if it is still at A).
//!
//! A third, read-only operation, listing refs by prefix, exists so the
//! engine can enumerate branches and anchors without a separate index.
//! Every practical backend (memory, SQLite, Postgres, object storage with
//! prefix listing, Durable Object storage) supports it trivially.
//!
//! The trait is deliberately **not** `Send`-bounded: futures on
//! wasm32-unknown-unknown (e.g. inside a Cloudflare Durable Object) are
//! `!Send`, and forked-core must run there unchanged. Native adapters are
//! free to be `Send + Sync`; the trait just does not require it.

use std::sync::Arc;

use crate::{Hash, StorageError};

/// Backend-agnostic storage for forked.
///
/// Implementations must guarantee:
/// - `put_blob` is idempotent: writing the same hash twice is a no-op.
/// - `cas_ref` is atomic: of any set of concurrent calls with the same
///   `name` and `expected`, at most one returns `true`.
#[allow(async_fn_in_trait)] // intentional: no Send bound, wasm futures are !Send
pub trait Storage {
    /// Fetch an immutable blob by its content hash. `None` if absent.
    async fn get_blob(&self, hash: &Hash) -> Result<Option<Vec<u8>>, StorageError>;

    /// Store an immutable blob under its content hash. Idempotent; an
    /// implementation may verify that `hash` matches `bytes`.
    async fn put_blob(&self, hash: &Hash, bytes: &[u8]) -> Result<(), StorageError>;

    /// Read the current value of a ref. `None` if the ref does not exist.
    async fn get_ref(&self, name: &str) -> Result<Option<Hash>, StorageError>;

    /// Atomically move a ref from `expected` to `new`.
    ///
    /// `expected == None` means "create; fail if the ref already exists".
    /// Returns `true` on success, `false` if the current value did not
    /// match `expected` (the caller lost a race).
    async fn cas_ref(
        &self,
        name: &str,
        expected: Option<&Hash>,
        new: &Hash,
    ) -> Result<bool, StorageError>;

    /// List all refs whose name starts with `prefix`, with their values.
    /// Order is unspecified.
    async fn list_refs(&self, prefix: &str) -> Result<Vec<(String, Hash)>, StorageError>;
}

impl<S: Storage> Storage for &S {
    async fn get_blob(&self, hash: &Hash) -> Result<Option<Vec<u8>>, StorageError> {
        (**self).get_blob(hash).await
    }

    async fn put_blob(&self, hash: &Hash, bytes: &[u8]) -> Result<(), StorageError> {
        (**self).put_blob(hash, bytes).await
    }

    async fn get_ref(&self, name: &str) -> Result<Option<Hash>, StorageError> {
        (**self).get_ref(name).await
    }

    async fn cas_ref(
        &self,
        name: &str,
        expected: Option<&Hash>,
        new: &Hash,
    ) -> Result<bool, StorageError> {
        (**self).cas_ref(name, expected, new).await
    }

    async fn list_refs(&self, prefix: &str) -> Result<Vec<(String, Hash)>, StorageError> {
        (**self).list_refs(prefix).await
    }
}

impl<S: Storage> Storage for Arc<S> {
    async fn get_blob(&self, hash: &Hash) -> Result<Option<Vec<u8>>, StorageError> {
        (**self).get_blob(hash).await
    }

    async fn put_blob(&self, hash: &Hash, bytes: &[u8]) -> Result<(), StorageError> {
        (**self).put_blob(hash, bytes).await
    }

    async fn get_ref(&self, name: &str) -> Result<Option<Hash>, StorageError> {
        (**self).get_ref(name).await
    }

    async fn cas_ref(
        &self,
        name: &str,
        expected: Option<&Hash>,
        new: &Hash,
    ) -> Result<bool, StorageError> {
        (**self).cas_ref(name, expected, new).await
    }

    async fn list_refs(&self, prefix: &str) -> Result<Vec<(String, Hash)>, StorageError> {
        (**self).list_refs(prefix).await
    }
}
