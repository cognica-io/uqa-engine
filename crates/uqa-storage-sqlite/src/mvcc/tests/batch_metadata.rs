//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered batch conditions share physical admission while preserving each key's original revision.

use super::*;
use uqa_storage::{mvcc::VersionedSessionOptions, KeyValueStore};

#[test]
fn ordered_batch_conditions_use_one_read_window_across_resident_and_spilled_edits() {
    for count in [4_u64, 128, 512] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("batch.db");
        let store = crate::key_value::SQLiteKeyValueStore::with_options(
            ManagedConnection::open(&path).unwrap(),
            VersionedSessionOptions {
                retained_bytes: 128 << 10,
            },
        )
        .unwrap();
        store.begin_transaction().unwrap();
        store.put(b"kept", b"before").unwrap();
        let before = RECORD_READS.with(std::cell::Cell::get);
        store
            .with_mutation(&mut |_, batch| {
                for id in 0..count {
                    batch.put(&id.to_be_bytes(), &[id as u8; 512])?;
                }
                Ok(())
            })
            .unwrap();
        assert_eq!(
            RECORD_READS.with(std::cell::Cell::get) - before,
            1,
            "{count} conditions"
        );
        for id in 0..count {
            assert_eq!(
                store.get(&id.to_be_bytes()).unwrap().unwrap(),
                [id as u8; 512]
            );
        }
        store.commit_transaction().unwrap();
        drop(store);
        let reopened = crate::key_value::SQLiteKeyValueStore::open(&path).unwrap();
        assert_eq!(reopened.get(b"kept").unwrap().unwrap(), b"before");
        for id in 0..count {
            assert_eq!(
                reopened.get(&id.to_be_bytes()).unwrap().unwrap(),
                [id as u8; 512]
            );
        }
    }
}

#[test]
fn ordered_batch_conditions_preserve_duplicate_prefix_undo_and_conflict_order() {
    let directory = tempfile::tempdir().unwrap();
    let store =
        crate::key_value::SQLiteKeyValueStore::open(&directory.path().join("order.db")).unwrap();
    for (key, value) in [("group/a", "a"), ("group/b", "b"), ("conflict", "original")] {
        store.put(key.as_bytes(), value.as_bytes()).unwrap();
    }
    let peer = store.new_session();
    store.begin_transaction().unwrap();
    store.put(b"private", b"kept").unwrap();
    store.savepoint("before").unwrap();
    store
        .with_mutation(&mut |_, batch| {
            batch.put(b"group/new", b"new")?;
            batch.put(b"group/a", b"first")?;
            batch.put(b"group/a", b"second")?;
            batch.delete(b"group/b")?;
            batch.delete_prefix(b"group/")?;
            batch.put(b"group/a", b"restored")?;
            batch.put(b"after", b"fresh")
        })
        .unwrap();
    assert_eq!(store.get(b"group/a").unwrap().unwrap(), b"restored");
    assert!(store.get(b"group/b").unwrap().is_none());
    assert!(store.get(b"group/new").unwrap().is_none());
    assert_eq!(store.get(b"after").unwrap().unwrap(), b"fresh");
    store.rollback_to_savepoint("before").unwrap();
    store.release_savepoint("before").unwrap();
    assert_eq!(store.get(b"group/a").unwrap().unwrap(), b"a");
    assert_eq!(store.get(b"group/b").unwrap().unwrap(), b"b");
    assert!(store.get(b"after").unwrap().is_none());
    assert!(store.get(b"group/new").unwrap().is_none());
    assert_eq!(store.get(b"private").unwrap().unwrap(), b"kept");

    // The later peer revision must not become the condition of a batch on the original snapshot.
    peer.put(b"conflict", b"peer").unwrap();
    store
        .with_mutation(&mut |_, batch| {
            batch.put(b"conflict", b"ours")?;
            batch.put(b"conflict", b"last")
        })
        .unwrap();
    let error = store.commit_transaction().unwrap_err();
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(&error);
    let mut conflict = false;
    while let Some(error) = source {
        conflict |= matches!(
            error.downcast_ref::<VersionError>(),
            Some(VersionError::WriteConflict { .. })
        );
        source = error.source();
    }
    assert!(conflict, "{error}");
    store.rollback_transaction().unwrap();
    assert_eq!(peer.get(b"conflict").unwrap().unwrap(), b"peer");
    assert!(peer.get(b"private").unwrap().is_none());
}

#[test]
fn private_only_batches_do_not_admit_a_committed_reader() {
    let directory = tempfile::tempdir().unwrap();
    let store =
        crate::key_value::SQLiteKeyValueStore::open(&directory.path().join("private.db")).unwrap();
    store.begin_transaction().unwrap();
    store.put(b"private", b"first").unwrap();
    let before = RECORD_READS.with(std::cell::Cell::get);
    store
        .with_mutation(&mut |_, batch| {
            for _ in 0..32 {
                batch.delete(b"private")?;
                batch.put(b"private", b"last")?;
            }
            Ok(())
        })
        .unwrap();
    assert_eq!(RECORD_READS.with(std::cell::Cell::get), before);
    assert_eq!(store.get(b"private").unwrap().unwrap(), b"last");
    store.rollback_transaction().unwrap();
    assert!(store.get(b"private").unwrap().is_none());
}
