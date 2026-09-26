//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Historical values survive live snapshots but disappear after their final leases end.

use super::*;
use std::path::Path;

#[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
mod process;
mod tombstones;

pub(super) fn open(path: &Path, mode: usize) -> ManagedConnection {
    match mode {
        0 => ManagedConnection::open(path),
        1 => ManagedConnection::open_encrypted(path, "snapshot-retention-test"),
        2 => ManagedConnection::open_compressed(path, crate::SQLiteCompressionOptions::default()),
        _ => ManagedConnection::open_compressed_encrypted(
            path,
            "snapshot-retention-test",
            crate::SQLiteCompressionOptions::default(),
        ),
    }
    .unwrap()
}

#[test]
fn reclamation_preserves_snapshots_tombstones_and_receipts_in_every_file_mode() {
    for mode in 0..4 {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("retention.db");
        {
            let connection = open(&path, mode);
            let store = SQLiteRecordStore::new(&connection).unwrap();
            uqa_storage::mvcc::verify_version_reclamation(&store).unwrap();
            connection
                .with(|connection| {
                    for (table, expected) in [
                        ("_uqa_mvcc_versions", 1),
                        ("_uqa_mvcc_heads", 2),
                        ("_uqa_mvcc_transactions", 4),
                    ] {
                        let count: i64 = connection.query_row(
                            &format!("SELECT count(*) FROM {table}"),
                            [],
                            |row| row.get(0),
                        )?;
                        assert_eq!(count, expected);
                    }
                    Ok(())
                })
                .unwrap();
        }
        let connection = open(&path, mode);
        let store = SQLiteRecordStore::new(&connection).unwrap();
        let snapshot = store.snapshot(&control()).unwrap();
        assert!(snapshot
            .get(b"retention-a", &control())
            .unwrap()
            .unwrap()
            .value()
            .is_none());
        assert_eq!(
            &***snapshot
                .get(b"retention-b", &control())
                .unwrap()
                .unwrap()
                .value()
                .unwrap(),
            b"later"
        );
        assert_eq!(store.reclaim_versions(&control()).unwrap(), 0);
    }
}

#[test]
fn independent_pools_and_in_memory_store_handles_share_snapshot_admission() {
    for memory in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("independent.db");
        let first = if memory {
            ManagedConnection::open_in_memory().unwrap()
        } else {
            open(&path, 0)
        };
        let a = SQLiteRecordStore::new(&first).unwrap();
        let other = if memory {
            first.new_session()
        } else {
            open(&path, 0)
        };
        let b = SQLiteRecordStore::new(&other).unwrap();
        assert!(Arc::ptr_eq(&a.snapshots, &b.snapshots));
        let control = control();
        let first_id = a.allocate_transaction(&control).unwrap();
        let receipt = a
            .commit(first_id, &prepared(b"key", b"old", &control), &control)
            .unwrap();
        let snapshot = a.snapshot(&control).unwrap();
        let changed = PreparedRecordCommit::new(
            &[RecordWrite {
                key: b"key",
                expected: Some(receipt.sequence),
                value: Some(b"new"),
            }],
            &control,
        )
        .unwrap();
        let id = b.allocate_transaction(&control).unwrap();
        b.commit(id, &changed, &control).unwrap();
        assert_eq!(b.reclaim_versions(&control).unwrap(), 0);
        assert_eq!(
            &***snapshot
                .get(b"key", &control)
                .unwrap()
                .unwrap()
                .value()
                .unwrap(),
            b"old"
        );
        drop(a);
        drop(first);
        assert_eq!(b.reclaim_versions(&control).unwrap(), 0);
        drop(snapshot);
        assert_eq!(b.reclaim_versions(&control).unwrap(), 1);
    }
}

#[test]
fn native_document_snapshots_survive_physical_vacuum_in_every_file_mode() {
    use std::collections::BTreeMap;
    use uqa_core::Value;
    use uqa_storage::DocumentStore;
    for mode in 0..4 {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("native-retention.db");
        let connection = open(&path, mode);
        crate::Catalog::open(connection.clone()).unwrap();
        connection
            .bind_native_records(uqa_storage::mvcc::VersionedSessionOptions::default())
            .unwrap();
        let mut documents = crate::SQLiteDocumentStore::new(connection.clone(), "docs");
        let old = BTreeMap::from([("body".into(), Value::Str("old".repeat(4096)))]);
        let new = BTreeMap::from([("body".into(), Value::Str("new".into()))]);
        documents.put(1, old.clone()).unwrap();
        let retained = documents.snapshot().unwrap();
        documents.put(1, new.clone()).unwrap();
        documents.put(2, new.clone()).unwrap();
        connection.vacuum().unwrap();
        assert_eq!(retained.get(1).unwrap(), Some(old));
        assert_eq!(retained.get(2).unwrap(), None);
        assert_eq!(documents.get(1).unwrap(), Some(new.clone()));
        let bytes = || {
            connection
                .with_physical(|connection| {
                    Ok(connection.query_row(
                        "SELECT coalesce(sum(length(value)), 0) FROM _uqa_mvcc_versions",
                        [],
                        |row| row.get::<_, i64>(0),
                    )?)
                })
                .unwrap()
        };
        let before = bytes();
        drop(retained);
        connection.vacuum().unwrap();
        assert!(bytes() < before / 2);
        assert_eq!(documents.get(1).unwrap(), Some(new));
    }
}

#[test]
fn interrupted_reclamation_rolls_back_all_deletions_and_keeps_receipts() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let store = SQLiteRecordStore::new(&connection).unwrap();
    let control = control();
    let initial = PreparedRecordCommit::new(
        &[
            RecordWrite {
                key: b"a",
                expected: None,
                value: Some(b"old"),
            },
            RecordWrite {
                key: b"b",
                expected: None,
                value: Some(b"old"),
            },
        ],
        &control,
    )
    .unwrap();
    let id = store.allocate_transaction(&control).unwrap();
    let receipt = store.commit(id, &initial, &control).unwrap();
    let next = PreparedRecordCommit::new(
        &[
            RecordWrite {
                key: b"a",
                expected: Some(receipt.sequence),
                value: Some(b"new"),
            },
            RecordWrite {
                key: b"b",
                expected: Some(receipt.sequence),
                value: Some(b"new"),
            },
        ],
        &control,
    )
    .unwrap();
    let next_id = store.allocate_transaction(&control).unwrap();
    store.commit(next_id, &next, &control).unwrap();
    store.with(|connection| {
        connection.execute_batch("CREATE TRIGGER reject_collection BEFORE DELETE ON _uqa_mvcc_versions WHEN OLD.key = x'62' BEGIN SELECT RAISE(ABORT, 'injected collection failure'); END")?;
        Ok(())
    }).unwrap();
    assert!(store.reclaim_versions(&control).is_err());
    store
        .with(|connection| {
            assert_eq!(
                connection
                    .query_row("SELECT count(*) FROM _uqa_mvcc_versions", [], |row| row
                        .get::<_, i64>(0))?,
                4
            );
            connection.execute_batch("DROP TRIGGER reject_collection")?;
            Ok(())
        })
        .unwrap();
    assert_eq!(
        store.commit_status(id, &control).unwrap(),
        CommitStatus::Committed(receipt)
    );
    assert_eq!(store.reclaim_versions(&control).unwrap(), 2);
    assert_eq!(store.commit(id, &initial, &control).unwrap(), receipt);
}

#[test]
fn compacted_tombstones_restore_history_when_the_key_is_reinserted() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let store = SQLiteRecordStore::new(&connection).unwrap();
    let control = control();
    let write = |expected, value: Option<&[u8]>| {
        let prepared = PreparedRecordCommit::new(
            &[RecordWrite {
                key: b"key",
                expected,
                value,
            }],
            &control,
        )
        .unwrap();
        store
            .commit(
                store.allocate_transaction(&control).unwrap(),
                &prepared,
                &control,
            )
            .unwrap()
            .sequence
    };
    let first = write(None, Some(b"old"));
    let removed = write(Some(first), None);
    let tombstone = store.snapshot(&control).unwrap();
    assert_eq!(store.reclaim_versions(&control).unwrap(), 1);
    store
        .with(|connection| {
            assert_eq!(
                connection
                    .query_row("SELECT count(*) FROM _uqa_mvcc_versions", [], |row| row
                        .get::<_, i64>(0))?,
                0
            );
            Ok(())
        })
        .unwrap();
    let replaced = write(Some(removed), Some(b"new"));
    let live = store.snapshot(&control).unwrap();
    assert_eq!(
        tombstone
            .metadata(b"key", &control)
            .unwrap()
            .unwrap()
            .revision,
        Some(removed)
    );
    assert!(tombstone
        .get(b"key", &control)
        .unwrap()
        .unwrap()
        .value()
        .is_none());
    assert!(tombstone.scan(b"", None, 8, &control).unwrap()[0]
        .version
        .value()
        .is_none());
    write(Some(replaced), None);
    assert_eq!(store.reclaim_versions(&control).unwrap(), 0);
    drop(tombstone);
    assert_eq!(store.reclaim_versions(&control).unwrap(), 1);
    assert_eq!(
        &***live
            .get(b"key", &control)
            .unwrap()
            .unwrap()
            .value()
            .unwrap(),
        b"new"
    );
    drop(live);
    assert_eq!(store.reclaim_versions(&control).unwrap(), 1);
    assert!(store
        .snapshot(&control)
        .unwrap()
        .get(b"key", &control)
        .unwrap()
        .unwrap()
        .value()
        .is_none());
}
