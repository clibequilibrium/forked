//! # forked-sqlite
//!
//! SQLite [`Storage`] adapter for forked, built on `rusqlite` with the
//! bundled SQLite (no system dependency). One file = one store; suitable
//! for the self-hosted server and local tooling.
//!
//! Calls execute synchronously on the calling thread behind a mutex -
//! individual statements are microseconds, which is fine for the MVP
//! server. A pooled/async variant can come later without touching the
//! format: the on-disk tables are just `blobs(hash, bytes)` and
//! `refs(name, hash)` as specified in `docs/FORMAT.md`.

#![forbid(unsafe_code)]

use std::path::Path;
use std::sync::{Mutex, PoisonError};

use forked_core::{Hash, Storage, StorageError};
use rusqlite::{params, Connection, OptionalExtension};

/// A SQLite-backed storage adapter.
pub struct SqliteStorage {
    conn: Mutex<Connection>,
}

fn db_err(e: rusqlite::Error) -> StorageError {
    StorageError::new(format!("sqlite: {e}"))
}

impl SqliteStorage {
    /// Open (or create) a store at `path`.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        Self::from_connection(Connection::open(path).map_err(db_err)?)
    }

    /// Open an in-memory store (handy for tests).
    pub fn open_in_memory() -> Result<Self, StorageError> {
        Self::from_connection(Connection::open_in_memory().map_err(db_err)?)
    }

    fn from_connection(conn: Connection) -> Result<Self, StorageError> {
        conn.pragma_update(None, "journal_mode", "WAL").ok();
        conn.pragma_update(None, "synchronous", "NORMAL").ok();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS blobs (
                 hash  BLOB PRIMARY KEY,
                 bytes BLOB NOT NULL
             ) WITHOUT ROWID;
             CREATE TABLE IF NOT EXISTS refs (
                 name TEXT PRIMARY KEY,
                 hash BLOB NOT NULL
             ) WITHOUT ROWID;",
        )
        .map_err(db_err)?;
        Ok(SqliteStorage {
            conn: Mutex::new(conn),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn hash_from_row(bytes: Vec<u8>) -> Result<Hash, StorageError> {
    let arr: [u8; 32] = bytes
        .try_into()
        .map_err(|_| StorageError::new("sqlite: stored ref hash is not 32 bytes"))?;
    Ok(Hash(arr))
}

impl Storage for SqliteStorage {
    async fn get_blob(&self, hash: &Hash) -> Result<Option<Vec<u8>>, StorageError> {
        self.lock()
            .query_row(
                "SELECT bytes FROM blobs WHERE hash = ?1",
                params![hash.as_bytes().as_slice()],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .optional()
            .map_err(db_err)
    }

    async fn put_blob(&self, hash: &Hash, bytes: &[u8]) -> Result<(), StorageError> {
        if *hash != Hash::of(bytes) {
            return Err(StorageError::new("blob bytes do not match hash"));
        }
        self.lock()
            .execute(
                "INSERT OR IGNORE INTO blobs (hash, bytes) VALUES (?1, ?2)",
                params![hash.as_bytes().as_slice(), bytes],
            )
            .map_err(db_err)?;
        Ok(())
    }

    async fn get_ref(&self, name: &str) -> Result<Option<Hash>, StorageError> {
        let row = self
            .lock()
            .query_row(
                "SELECT hash FROM refs WHERE name = ?1",
                params![name],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .optional()
            .map_err(db_err)?;
        row.map(hash_from_row).transpose()
    }

    async fn cas_ref(
        &self,
        name: &str,
        expected: Option<&Hash>,
        new: &Hash,
    ) -> Result<bool, StorageError> {
        // Single-statement CAS in both directions; `changes()` tells us
        // whether we won.
        let changed = match expected {
            None => self
                .lock()
                .execute(
                    "INSERT OR IGNORE INTO refs (name, hash) VALUES (?1, ?2)",
                    params![name, new.as_bytes().as_slice()],
                )
                .map_err(db_err)?,
            Some(expected) => self
                .lock()
                .execute(
                    "UPDATE refs SET hash = ?3 WHERE name = ?1 AND hash = ?2",
                    params![
                        name,
                        expected.as_bytes().as_slice(),
                        new.as_bytes().as_slice()
                    ],
                )
                .map_err(db_err)?,
        };
        Ok(changed == 1)
    }

    async fn list_refs(&self, prefix: &str) -> Result<Vec<(String, Hash)>, StorageError> {
        // Range scan: every allowed ref-name byte is < 0x7F, so
        // `prefix + \x7F` upper-bounds all names with this prefix.
        let upper = format!("{prefix}\u{7F}");
        let conn = self.lock();
        let mut stmt = conn
            .prepare("SELECT name, hash FROM refs WHERE name >= ?1 AND name < ?2")
            .map_err(db_err)?;
        let rows = stmt
            .query_map(params![prefix, upper], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?))
            })
            .map_err(db_err)?;
        let mut out = Vec::new();
        for row in rows {
            let (name, bytes) = row.map_err(db_err)?;
            out.push((name, hash_from_row(bytes)?));
        }
        Ok(out)
    }
}
