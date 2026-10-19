//! # forked-memory
//!
//! In-memory [`Storage`] adapter for forked. Used by the test suites and
//! for local development; everything lives in two maps behind a mutex.

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, HashMap};
use std::sync::{Mutex, PoisonError};

use forked_core::{Hash, Storage, StorageError};

#[derive(Default)]
struct Inner {
    blobs: HashMap<Hash, Vec<u8>>,
    refs: BTreeMap<String, Hash>,
}

/// An in-memory storage backend.
///
/// Thread-safe (a mutex guards both tables) and cheap: blobs are stored
/// once per content hash, so structural sharing in forked-core translates
/// directly into memory savings here.
#[derive(Default)]
pub struct MemoryStorage {
    inner: Mutex<Inner>,
}

impl MemoryStorage {
    /// Create an empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of distinct blobs stored. Useful in tests asserting that
    /// forks and small updates do not copy state.
    pub fn blob_count(&self) -> usize {
        self.lock().blobs.len()
    }

    /// Number of refs (branch heads and anchors).
    pub fn ref_count(&self) -> usize {
        self.lock().refs.len()
    }

    /// Total bytes across all stored blobs.
    pub fn blob_bytes(&self) -> usize {
        self.lock().blobs.values().map(Vec::len).sum()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        // A poisoned mutex only means another thread panicked mid-access;
        // both tables are always internally consistent, so recover.
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Storage for MemoryStorage {
    async fn get_blob(&self, hash: &Hash) -> Result<Option<Vec<u8>>, StorageError> {
        Ok(self.lock().blobs.get(hash).cloned())
    }

    async fn put_blob(&self, hash: &Hash, bytes: &[u8]) -> Result<(), StorageError> {
        if *hash != Hash::of(bytes) {
            return Err(StorageError::new("blob bytes do not match hash"));
        }
        self.lock()
            .blobs
            .entry(*hash)
            .or_insert_with(|| bytes.to_vec());
        Ok(())
    }

    async fn get_ref(&self, name: &str) -> Result<Option<Hash>, StorageError> {
        Ok(self.lock().refs.get(name).copied())
    }

    async fn cas_ref(
        &self,
        name: &str,
        expected: Option<&Hash>,
        new: &Hash,
    ) -> Result<bool, StorageError> {
        let mut inner = self.lock();
        let current = inner.refs.get(name);
        let matches = match (expected, current) {
            (None, None) => true,
            (Some(e), Some(c)) => e == c,
            _ => false,
        };
        if matches {
            inner.refs.insert(name.to_string(), *new);
        }
        Ok(matches)
    }

    async fn list_refs(&self, prefix: &str) -> Result<Vec<(String, Hash)>, StorageError> {
        Ok(self
            .lock()
            .refs
            .range(prefix.to_string()..)
            .take_while(|(name, _)| name.starts_with(prefix))
            .map(|(name, hash)| (name.clone(), *hash))
            .collect())
    }
}
