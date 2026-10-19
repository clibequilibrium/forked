//! # forked-core
//!
//! Versioned state for AI agents: fork, rewind and replay structured agent
//! state (messages, memory, plans, tool results) the way git versions
//! files.
//!
//! - **Sessions** hold branches; **branches** are named refs pointing at a
//!   head **commit**; commits are immutable and chain through parents.
//! - **State** is a key→JSON-value map stored as a persistent,
//!   structurally shared, content-addressed tree: forking is O(1), an
//!   update rewrites only the touched path.
//! - **Effects** (emails, payments, API calls) are recorded inside commits
//!   by idempotency key. Rewind never undoes an effect; re-recording a
//!   known key returns the stored result instead of re-running it.
//!
//! The crate is backend-agnostic via the [`Storage`] trait (blob get/put by
//! hash + compare-and-swap on refs) and compiles for
//! `wasm32-unknown-unknown`: no threads, no OS I/O, no `tokio`, no `Send`
//! bounds on futures, and no panics in normal paths.
//!
//! ```
//! use forked_core::{CommitInput, Forked, Op};
//!
//! # async fn demo<S: forked_core::Storage>(storage: S) -> Result<(), forked_core::Error> {
//! let forked = Forked::new(storage);
//! forked.create_session("sess-1").await?;
//! forked
//!     .commit(
//!         "sess-1",
//!         "main",
//!         CommitInput {
//!             ops: vec![Op::Put("plan".into(), "draft the email".into())],
//!             ..Default::default()
//!         },
//!     )
//!     .await?;
//! forked.fork("sess-1", "main", "alt").await?; // O(1)
//! # Ok(())
//! # }
//! ```
//!
//! The on-disk format is specified in `docs/FORMAT.md` at the repository
//! root.

#![forbid(unsafe_code)]

mod commit;
mod diff;
mod encoding;
mod error;
mod hash;
mod session;
mod storage;
mod tree;

pub use commit::{Commit, EffectRecord, LogEntry, Meta};
pub use diff::{Change, DiffEntry};
pub use error::{Error, Result, StorageError};
pub use hash::Hash;
pub use session::{CommitInput, EffectOutcome, Forked, Op, SessionGraph, DEFAULT_BRANCH};
pub use storage::Storage;

/// State values are JSON values; maps serialize with sorted keys, keeping
/// the encoding deterministic.
pub use serde_json::Value;
