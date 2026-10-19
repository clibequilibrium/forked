//! Integration tests for the forked engine over the in-memory adapter,
//! covering the guarantees promised in the spec: cheap forks, exact
//! rewinds, CAS conflicts on concurrent commits, effects surviving rewind,
//! structural sharing, and canonical (order-independent) root hashes.

use std::sync::Arc;

use forked_core::{Change, CommitInput, EffectOutcome, EffectRecord, Error, Forked, Op, Value};
use forked_memory::MemoryStorage;
use pollster::block_on;
use serde_json::json;

fn engine() -> Forked<MemoryStorage> {
    Forked::new(MemoryStorage::new())
}

fn put(key: &str, value: Value) -> CommitInput {
    CommitInput {
        ops: vec![Op::Put(key.to_string(), value)],
        ..Default::default()
    }
}

fn puts(entries: Vec<(String, Value)>) -> CommitInput {
    CommitInput {
        ops: entries.into_iter().map(|(k, v)| Op::Put(k, v)).collect(),
        ..Default::default()
    }
}

#[test]
fn forking_1000_branches_stays_cheap() {
    block_on(async {
        let f = engine();
        f.create_session("s").await.unwrap();
        // Give the session a non-trivial state first.
        let entries = (0..100)
            .map(|i| (format!("key-{i}"), json!({ "n": i, "text": "payload" })))
            .collect();
        f.commit("s", "main", puts(entries)).await.unwrap();

        let blobs_before = f.storage().blob_count();
        let refs_before = f.storage().ref_count();
        for i in 0..1000 {
            f.fork("s", "main", &format!("branch-{i}")).await.unwrap();
        }
        // A fork writes one ref and zero blobs: no state is copied.
        assert_eq!(f.storage().blob_count(), blobs_before);
        assert_eq!(f.storage().ref_count(), refs_before + 1000);

        // Each fork sees the full state.
        let v = f.get("s", "branch-999", "key-42").await.unwrap();
        assert_eq!(v, Some(json!({ "n": 42, "text": "payload" })));
    });
}

#[test]
fn rewind_restores_exact_state() {
    block_on(async {
        let f = engine();
        f.create_session("s").await.unwrap();
        f.commit("s", "main", put("plan", json!("step 1")))
            .await
            .unwrap();
        let target = f
            .commit(
                "s",
                "main",
                puts(vec![
                    ("plan".into(), json!("step 2")),
                    ("notes".into(), json!(["a", "b"])),
                ]),
            )
            .await
            .unwrap();
        let wanted = f.snapshot("s", "main").await.unwrap();

        // Diverge: overwrite, add, delete.
        f.commit(
            "s",
            "main",
            CommitInput {
                ops: vec![
                    Op::Put("plan".into(), json!("step 3 (wrong turn)")),
                    Op::Put("scratch".into(), json!(123)),
                    Op::Delete("notes".into()),
                ],
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_ne!(f.snapshot("s", "main").await.unwrap(), wanted);

        f.checkout("s", "main", &target).await.unwrap();
        assert_eq!(f.snapshot("s", "main").await.unwrap(), wanted);
        assert_eq!(f.head("s", "main").await.unwrap(), target);
    });
}

#[test]
fn concurrent_commits_one_wins() {
    block_on(async {
        let f = engine();
        f.create_session("s").await.unwrap();
        let head = f.head("s", "main").await.unwrap();

        // Two writers both built on the same head: the CAS lets exactly one
        // through and the other gets a conflict.
        let first = f.commit_at("s", "main", &head, put("x", json!(1))).await;
        let second = f.commit_at("s", "main", &head, put("x", json!(2))).await;
        assert!(first.is_ok());
        assert!(matches!(second, Err(Error::Conflict(_))));
        assert_eq!(f.get("s", "main", "x").await.unwrap(), Some(json!(1)));
    });
}

#[test]
fn concurrent_commits_race_across_threads() {
    let storage = Arc::new(MemoryStorage::new());
    let head = block_on(async {
        let f = Forked::new(Arc::clone(&storage));
        f.create_session("s").await.unwrap();
        f.head("s", "main").await.unwrap()
    });

    let handles: Vec<_> = (0..8)
        .map(|i| {
            let storage = Arc::clone(&storage);
            std::thread::spawn(move || {
                let f = Forked::new(storage);
                block_on(f.commit_at("s", "main", &head, put("winner", json!(i))))
            })
        })
        .collect();
    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();

    let wins = results.iter().filter(|r| r.is_ok()).count();
    let conflicts = results
        .iter()
        .filter(|r| matches!(r, Err(Error::Conflict(_))))
        .count();
    assert_eq!(wins, 1, "exactly one concurrent commit must win");
    assert_eq!(conflicts, 7);
}

#[test]
fn effect_still_recorded_after_rewind() {
    block_on(async {
        let f = engine();
        f.create_session("s").await.unwrap();
        let before = f
            .commit("s", "main", put("draft", json!("v1")))
            .await
            .unwrap();

        let effect = EffectRecord {
            key: "send-email:42".into(),
            kind: "email.send".into(),
            payload: json!({ "to": "user@example.com" }),
            result: json!({ "message_id": "abc-123" }),
        };
        let outcome = f.record_effect("s", "main", effect.clone()).await.unwrap();
        assert!(matches!(outcome, EffectOutcome::Recorded { .. }));

        // Rewind past the effect commit, the email was still sent.
        f.checkout("s", "main", &before).await.unwrap();
        let replay = f.record_effect("s", "main", effect.clone()).await.unwrap();
        match replay {
            EffectOutcome::AlreadyRecorded(record) => {
                assert_eq!(record.result, json!({ "message_id": "abc-123" }));
            }
            other => panic!("expected AlreadyRecorded, got {other:?}"),
        }
        // And it is visible on a fork of the rewound branch too.
        f.fork("s", "main", "alt").await.unwrap();
        let on_fork = f.record_effect("s", "alt", effect).await.unwrap();
        assert!(matches!(on_fork, EffectOutcome::AlreadyRecorded(_)));
    });
}

#[test]
fn small_update_shares_structure() {
    block_on(async {
        let f = engine();
        f.create_session("s").await.unwrap();
        let entries = (0..1000).map(|i| (format!("key-{i}"), json!(i))).collect();
        f.commit("s", "main", puts(entries)).await.unwrap();

        let before = f.storage().blob_count();
        f.commit("s", "main", put("key-500", json!("updated")))
            .await
            .unwrap();
        let new_blobs = f.storage().blob_count() - before;
        // One rewritten path (root → … → leaf) plus one commit object. With
        // 1000 keys the trie is ~2–3 levels deep.
        assert!(
            new_blobs <= 6,
            "expected a handful of new blobs, got {new_blobs}"
        );
    });
}

#[test]
fn same_contents_same_root_hash() {
    block_on(async {
        let f = engine();
        f.create_session("a").await.unwrap();
        f.create_session("b").await.unwrap();

        // Different insertion orders, interleaved deletes, same final map.
        f.commit(
            "a",
            "main",
            puts(vec![
                ("alpha".into(), json!(1)),
                ("beta".into(), json!(2)),
                ("gamma".into(), json!(3)),
            ]),
        )
        .await
        .unwrap();
        f.commit(
            "b",
            "main",
            CommitInput {
                ops: vec![
                    Op::Put("gamma".into(), json!(3)),
                    Op::Put("temp".into(), json!("x")),
                    Op::Put("beta".into(), json!(2)),
                    Op::Delete("temp".into()),
                    Op::Put("alpha".into(), json!(1)),
                ],
                ..Default::default()
            },
        )
        .await
        .unwrap();

        let root_a = f
            .read_commit(&f.head("a", "main").await.unwrap())
            .await
            .unwrap()
            .root;
        let root_b = f
            .read_commit(&f.head("b", "main").await.unwrap())
            .await
            .unwrap()
            .root;
        assert_eq!(root_a, root_b, "canonical trees must hash identically");

        // Deleting everything returns to the canonical empty root.
        f.commit(
            "a",
            "main",
            CommitInput {
                ops: vec![
                    Op::Delete("alpha".into()),
                    Op::Delete("beta".into()),
                    Op::Delete("gamma".into()),
                ],
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let log = f.log("a", "main", None).await.unwrap();
        let empty_root = log.last().unwrap().commit.root; // session root commit
        let now_root = log.first().unwrap().commit.root;
        assert_eq!(now_root, empty_root);
    });
}

#[test]
fn diff_reports_adds_removes_and_modifications() {
    block_on(async {
        let f = engine();
        f.create_session("s").await.unwrap();
        let a = f
            .commit(
                "s",
                "main",
                puts(vec![
                    ("keep".into(), json!("same")),
                    ("change".into(), json!("old")),
                    ("drop".into(), json!(true)),
                ]),
            )
            .await
            .unwrap();
        let b = f
            .commit(
                "s",
                "main",
                CommitInput {
                    ops: vec![
                        Op::Put("change".into(), json!("new")),
                        Op::Delete("drop".into()),
                        Op::Put("fresh".into(), json!([1, 2])),
                    ],
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        let diff = f.diff(&a, &b).await.unwrap();
        let by_key: Vec<(&str, &Change)> =
            diff.iter().map(|d| (d.key.as_str(), &d.change)).collect();
        assert_eq!(diff.len(), 3, "unexpected diff: {by_key:?}");
        assert!(matches!(
            &diff[0].change,
            Change::Modified { old, new } if old == &json!("old") && new == &json!("new")
        ));
        assert_eq!(diff[0].key, "change");
        assert!(matches!(&diff[1].change, Change::Removed { old } if old == &json!(true)));
        assert_eq!(diff[1].key, "drop");
        assert!(matches!(&diff[2].change, Change::Added { new } if new == &json!([1, 2])));
        assert_eq!(diff[2].key, "fresh");

        // Identical commits diff to nothing.
        assert!(f.diff(&b, &b).await.unwrap().is_empty());
    });
}

#[test]
fn log_walks_history_newest_first() {
    block_on(async {
        let f = engine();
        let root = f.create_session("s").await.unwrap();
        let c1 = f.commit("s", "main", put("a", json!(1))).await.unwrap();
        let c2 = f.commit("s", "main", put("b", json!(2))).await.unwrap();

        let log = f.log("s", "main", None).await.unwrap();
        let hashes: Vec<_> = log.iter().map(|e| e.hash).collect();
        assert_eq!(hashes, vec![c2, c1, root]);
        assert_eq!(log[2].commit.parent, None);

        let limited = f.log("s", "main", Some(1)).await.unwrap();
        assert_eq!(limited.len(), 1);
        assert_eq!(limited[0].hash, c2);
    });
}

#[test]
fn branches_are_independent_after_fork() {
    block_on(async {
        let f = engine();
        f.create_session("s").await.unwrap();
        f.commit("s", "main", put("shared", json!("base")))
            .await
            .unwrap();
        f.fork("s", "main", "experiment").await.unwrap();

        f.commit("s", "experiment", put("shared", json!("changed")))
            .await
            .unwrap();
        f.commit("s", "experiment", put("extra", json!(1)))
            .await
            .unwrap();

        assert_eq!(
            f.get("s", "main", "shared").await.unwrap(),
            Some(json!("base"))
        );
        assert_eq!(f.get("s", "main", "extra").await.unwrap(), None);
        assert_eq!(
            f.get("s", "experiment", "shared").await.unwrap(),
            Some(json!("changed"))
        );

        // Re-forking under the same name fails; forking from the fork works.
        assert!(matches!(
            f.fork("s", "main", "experiment").await,
            Err(Error::BranchExists(_))
        ));
        let names: Vec<String> = f
            .branches("s")
            .await
            .unwrap()
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        assert_eq!(names, vec!["experiment".to_string(), "main".to_string()]);
    });
}

#[test]
fn checkout_rejects_foreign_commits() {
    block_on(async {
        let f = engine();
        f.create_session("s1").await.unwrap();
        f.create_session("s2").await.unwrap();
        // Note: the two sessions' *root* commits are byte-identical and
        // therefore the very same content-addressed object, that one is
        // legitimately reachable from both. A commit with actual content in
        // s2 is foreign to s1 and must be rejected.
        let foreign = f.commit("s2", "main", put("x", json!(1))).await.unwrap();
        assert!(matches!(
            f.checkout("s1", "main", &foreign).await,
            Err(Error::NotFound(_))
        ));
        // And a bogus hash is rejected too.
        assert!(f
            .checkout("s1", "main", &forked_core::Hash([7u8; 32]))
            .await
            .is_err());
    });
}

#[test]
fn invalid_names_are_rejected() {
    block_on(async {
        let f = engine();
        assert!(matches!(
            f.create_session("has/slash").await,
            Err(Error::InvalidName(_))
        ));
        assert!(matches!(
            f.create_session("").await,
            Err(Error::InvalidName(_))
        ));
        f.create_session("ok-name_1.2").await.unwrap();
        assert!(matches!(
            f.fork("ok-name_1.2", "main", "bad name").await,
            Err(Error::InvalidName(_))
        ));
    });
}

#[test]
fn creating_a_session_twice_fails() {
    block_on(async {
        let f = engine();
        f.create_session("s").await.unwrap();
        assert!(matches!(
            f.create_session("s").await,
            Err(Error::SessionExists(_))
        ));
    });
}
