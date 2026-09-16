//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::sync::mpsc;
use std::time::Duration;

use redb::{Database, ReadableDatabase, TableDefinition};
use uqa_storage::KeyValueStore;
use uqa_storage_redb::RedbStorage;

#[test]
fn an_independent_key_value_writer_finishes_before_the_first_transaction_ends() {
    for ending in ["commit", "rollback", "savepoint"] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("writers.redb");
        {
            let storage = RedbStorage::open(&path).unwrap();
            let a = storage.store();
            a.begin_transaction().unwrap();
            a.put(b"a", b"first").unwrap();
            a.savepoint("keep").unwrap();
            a.put(b"a", b"second").unwrap();
            let b = storage.store();
            let (sent, received) = mpsc::channel();
            let other = std::thread::spawn(move || {
                b.begin_transaction().unwrap();
                b.put(b"b", b"other").unwrap();
                b.commit_transaction().unwrap();
                sent.send(()).unwrap();
            });
            let completed = received.recv_timeout(Duration::from_secs(5));
            if completed.is_err() {
                a.rollback_transaction().unwrap();
                other.join().unwrap();
                panic!("independent writer waited for the first transaction: {completed:?}");
            }
            other.join().unwrap();
            assert!(a.in_transaction());
            assert_eq!(a.get(b"b").unwrap(), None);
            assert_eq!(storage.store().get(b"b").unwrap().unwrap(), b"other");
            match ending {
                "commit" => a.commit_transaction().unwrap(),
                "rollback" => a.rollback_transaction().unwrap(),
                _ => {
                    a.rollback_to_savepoint("keep").unwrap();
                    a.commit_transaction().unwrap();
                }
            }
        }
        let storage = RedbStorage::open(&path).unwrap();
        let store = storage.store();
        assert_eq!(store.get(b"b").unwrap().unwrap(), b"other");
        let expected = match ending {
            "commit" => Some(b"second".as_slice()),
            "savepoint" => Some(b"first".as_slice()),
            _ => None,
        };
        assert_eq!(store.get(b"a").unwrap().as_deref(), expected);
    }
}

#[test]
fn a_stale_session_cannot_overwrite_another_commit_or_publish_partial_records() {
    let directory = tempfile::tempdir().unwrap();
    let storage = RedbStorage::open(directory.path().join("conflict.redb")).unwrap();
    let a = storage.store();
    let b = storage.store();
    a.put(b"z", b"initial").unwrap();
    a.begin_transaction().unwrap();
    a.put(b"a", b"private").unwrap();
    a.delete(b"z").unwrap();
    b.put(b"z", b"other").unwrap();
    assert!(a.commit_transaction().is_err());
    assert!(a.in_transaction());
    assert_eq!(b.get(b"a").unwrap(), None);
    a.rollback_transaction().unwrap();
    assert_eq!(a.get(b"z").unwrap().unwrap(), b"other");
}

#[test]
fn legacy_bytes_migrate_once_and_the_old_typed_table_can_no_longer_open() {
    const LEGACY: TableDefinition<&[u8], &[u8]> = TableDefinition::new("uqa_key_value");
    const META: TableDefinition<&str, u64> = TableDefinition::new("uqa_storage_metadata");
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("legacy.redb");
    let fixtures = [
        (b"".as_slice(), b"".as_slice()),
        (b"a\0", b"zero\0\xff"),
        (b"\xff", b"last"),
    ];
    {
        let database = Database::create(&path).unwrap();
        let transaction = database.begin_write().unwrap();
        {
            let mut table = transaction.open_table(LEGACY).unwrap();
            for (key, value) in fixtures {
                table.insert(key, value).unwrap();
            }
        }
        transaction
            .open_table(META)
            .unwrap()
            .insert("change_version", 42)
            .unwrap();
        transaction.commit().unwrap();
    }
    for opening in 0..2 {
        let storage = RedbStorage::open(&path).unwrap();
        let store = storage.store();
        for (key, value) in fixtures {
            assert_eq!(store.get(key).unwrap().as_deref(), Some(value));
        }
        assert_eq!(store.change_version().unwrap(), Some(42 + opening));
        store.put(b"new", b"after migration").unwrap();
    }
    let database = Database::create(&path).unwrap();
    assert!(database.begin_write().unwrap().open_table(LEGACY).is_err());
    let transaction = database.begin_read().unwrap();
    let guard: TableDefinition<u8, u8> = TableDefinition::new("uqa_key_value");
    assert_eq!(
        transaction
            .open_table(guard)
            .unwrap()
            .get(0)
            .unwrap()
            .unwrap()
            .value(),
        1
    );
}
