//! Reproducible numbers for the README:
//!
//! ```sh
//! cargo run --release -p forked-sqlite --example bench
//! ```
//!
//! Not a microbenchmark suite, a transparent, end-to-end measurement of
//! the claims forked makes: forks are O(1) ref writes, updates rewrite only
//! the touched path, rewinds are instant.

use std::time::Instant;

use forked_core::{CommitInput, Forked, Op, Storage, Value};
use forked_memory::MemoryStorage;
use forked_sqlite::SqliteStorage;

const KEYS: usize = 1_000;
const FORKS: usize = 1_000;

fn main() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("bench.db");

    println!("forked bench, {KEYS} state keys, {FORKS} forks\n");
    pollster::block_on(run(
        "sqlite (file, WAL)",
        SqliteStorage::open(&path).expect("open sqlite"),
    ));
    println!();
    pollster::block_on(run("in-memory", MemoryStorage::new()));
}

async fn run<S: Storage>(name: &str, storage: S) {
    let f = Forked::new(storage);
    f.create_session("bench").await.unwrap();

    // Seed: one commit with KEYS keys.
    let ops: Vec<Op> = (0..KEYS)
        .map(|i| {
            Op::Put(
                format!("key-{i}"),
                serde_json::json!({ "i": i, "text": "payload payload payload" }),
            )
        })
        .collect();
    let t = Instant::now();
    f.commit(
        "bench",
        "main",
        CommitInput {
            ops,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let seed = t.elapsed();

    // Fork FORKS branches off main.
    let t = Instant::now();
    for i in 0..FORKS {
        f.fork("bench", "main", &format!("b{i}")).await.unwrap();
    }
    let fork = t.elapsed();

    // Update a single key among KEYS.
    let t = Instant::now();
    let updated = f
        .commit(
            "bench",
            "main",
            CommitInput {
                ops: vec![Op::Put("key-500".into(), Value::from("updated"))],
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let update = t.elapsed();

    // Rewind main back one commit and read a key.
    let parent = f.read_commit(&updated).await.unwrap().parent.unwrap();
    let t = Instant::now();
    f.checkout("bench", "main", &parent).await.unwrap();
    let rewind = t.elapsed();
    let t = Instant::now();
    let value = f.get("bench", "main", "key-500").await.unwrap();
    let read = t.elapsed();
    assert_eq!(value.unwrap()["i"], 500);

    println!("backend: {name}");
    println!("  seed    {KEYS} keys in one commit            {seed:>10.2?}");
    println!(
        "  fork    {FORKS} branches                      {fork:>10.2?}  ({:.0?}/fork, 0 state bytes copied)",
        fork / FORKS as u32
    );
    println!("  update  1 key among {KEYS} (path rewrite)    {update:>10.2?}");
    println!("  rewind  branch head moved back            {rewind:>10.2?}");
    println!("  read    1 key after rewind                {read:>10.2?}");
}
