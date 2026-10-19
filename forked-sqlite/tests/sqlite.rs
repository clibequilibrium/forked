//! Adapter tests: CAS semantics, persistence across reopen, and parity
//! with the in-memory adapter (same engine ops ⇒ same hashes).

use forked_core::{CommitInput, Forked, Hash, Op, Storage};
use forked_memory::MemoryStorage;
use forked_sqlite::SqliteStorage;
use pollster::block_on;
use serde_json::json;

fn put(key: &str, value: serde_json::Value) -> CommitInput {
    CommitInput {
        ops: vec![Op::Put(key.to_string(), value)],
        ..Default::default()
    }
}

#[test]
fn cas_semantics() {
    block_on(async {
        let s = SqliteStorage::open_in_memory().unwrap();
        let a = Hash::of(b"a");
        let b = Hash::of(b"b");

        // Create requires absence.
        assert!(s.cas_ref("r", None, &a).await.unwrap());
        assert!(!s.cas_ref("r", None, &b).await.unwrap());
        // Swap requires the expected current value.
        assert!(!s.cas_ref("r", Some(&b), &a).await.unwrap());
        assert!(s.cas_ref("r", Some(&a), &b).await.unwrap());
        assert_eq!(s.get_ref("r").await.unwrap(), Some(b));
    });
}

#[test]
fn list_refs_respects_prefix_boundaries() {
    block_on(async {
        let s = SqliteStorage::open_in_memory().unwrap();
        let h = Hash::of(b"x");
        // "a_b" must not leak into a listing for "s/aXb/" even though `_`
        // is a SQL LIKE wildcard, the adapter must not use LIKE.
        for name in ["s/a_b/b/main", "s/aXb/b/main", "s/a_b/a/zzz"] {
            s.cas_ref(name, None, &h).await.unwrap();
        }
        let got = s.list_refs("s/a_b/").await.unwrap();
        let names: Vec<_> = got.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names.len(), 2);
        assert!(names.contains(&"s/a_b/b/main"));
        assert!(names.contains(&"s/a_b/a/zzz"));
    });
}

#[test]
fn persists_across_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("forked.db");

    let head = block_on(async {
        let f = Forked::new(SqliteStorage::open(&path).unwrap());
        f.create_session("s").await.unwrap();
        f.commit("s", "main", put("k", json!({"v": 1})))
            .await
            .unwrap()
    });

    block_on(async {
        let f = Forked::new(SqliteStorage::open(&path).unwrap());
        assert_eq!(f.head("s", "main").await.unwrap(), head);
        assert_eq!(
            f.get("s", "main", "k").await.unwrap(),
            Some(json!({"v": 1}))
        );
    });
}

#[test]
fn same_hashes_as_memory_adapter() {
    async fn drive<S: Storage>(f: &Forked<S>) -> Hash {
        f.create_session("s").await.unwrap();
        f.commit(
            "s",
            "main",
            CommitInput {
                ops: vec![
                    Op::Put("a".into(), json!([1, 2, 3])),
                    Op::Put("b".into(), json!({"nested": {"x": true}})),
                ],
                ..Default::default()
            },
        )
        .await
        .unwrap()
    }

    block_on(async {
        let mem = Forked::new(MemoryStorage::new());
        let sql = Forked::new(SqliteStorage::open_in_memory().unwrap());
        assert_eq!(drive(&mem).await, drive(&sql).await);
    });
}
