//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{open, MODES};
use std::{sync::mpsc, time::Duration};
use uqa_storage::{
    mvcc::VersionedSessionOptions, read_control::StorageReadControl, KeyValueStore,
    StorageBackendError,
};
use uqa_storage_sqlite::{
    Catalog, ManagedConnection, SQLiteError, SQLiteKeyValueStorage, SQLiteKeyValueStore,
};

#[test]
fn public_stores_commit_independently_of_another_connection_clones_private_writes() {
    for mode in MODES {
        for ending in ["commit", "rollback", "savepoint"] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("logical.db");
            let connection = open(mode, &path);
            let clone = connection.clone();
            let a = SQLiteKeyValueStore::new(connection).unwrap();
            clone.begin_transaction().unwrap();
            a.put(b"a", b"first").unwrap();
            clone.savepoint("keep").unwrap();
            a.put(b"a", b"second").unwrap();
            assert!(clone.transaction_has_written().unwrap());
            let other_path = path.clone();
            let (sent, received) = mpsc::channel();
            let writer = std::thread::spawn(move || {
                let b = SQLiteKeyValueStore::new(open(mode, &other_path)).unwrap();
                b.begin_transaction().unwrap();
                b.put(b"b", b"independent").unwrap();
                b.commit_transaction().unwrap();
                sent.send(()).unwrap();
            });
            let completed = received.recv_timeout(Duration::from_secs(20));
            if completed.is_err() {
                clone.rollback_transaction().unwrap();
                writer.join().unwrap();
                panic!("independent KeyValue writer did not finish: {mode:?} {completed:?}");
            }
            writer.join().unwrap();
            assert!(a.in_transaction());
            assert_eq!(a.get(b"a").unwrap().as_deref(), Some(b"second".as_slice()));
            assert_eq!(a.get(b"b").unwrap(), None);
            match ending {
                "commit" => clone.commit_transaction().unwrap(),
                "rollback" => clone.rollback_transaction().unwrap(),
                _ => {
                    clone.rollback_to_savepoint("keep").unwrap();
                    clone.commit_transaction().unwrap();
                }
            }
            assert!(!a.in_transaction());
            drop(a);
            drop(clone);
            let reopened = SQLiteKeyValueStore::new(open(mode, &path)).unwrap();
            let expected = match ending {
                "commit" => Some(b"second".as_slice()),
                "savepoint" => Some(b"first".as_slice()),
                _ => None,
            };
            assert_eq!(reopened.get(b"a").unwrap().as_deref(), expected);
            assert_eq!(
                reopened.get(b"b").unwrap().as_deref(),
                Some(b"independent".as_slice())
            );
        }
    }
}

#[test]
fn memory_sessions_and_rebound_handles_keep_one_logical_transaction_boundary() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let before_binding = connection.clone();
    let a = SQLiteKeyValueStore::new(connection).unwrap();
    let same = SQLiteKeyValueStore::new(before_binding.clone()).unwrap();
    let b = a.new_session();
    before_binding.begin_deferred_transaction().unwrap();
    same.put(b"a", b"private").unwrap();
    b.put(b"b", b"committed").unwrap();
    assert_eq!(a.get(b"a").unwrap().as_deref(), Some(b"private".as_slice()));
    assert_eq!(a.get(b"b").unwrap(), None);
    assert!(matches!(
        before_binding.with(|_| Ok(())),
        Err(SQLiteError::LogicalSessionRequired)
    ));
    assert!(matches!(
        before_binding.with_physical(|_| Ok(())),
        Err(SQLiteError::TransactionAlreadyActive)
    ));
    before_binding.rollback_transaction().unwrap();
    assert!(!same.in_transaction());
    assert_eq!(a.get(b"a").unwrap(), None);
    assert_eq!(
        a.get(b"b").unwrap().as_deref(),
        Some(b"committed".as_slice())
    );
    assert!(matches!(
        Catalog::open(before_binding.clone()),
        Err(SQLiteError::LogicalSessionRequired)
    ));
    assert!(matches!(
        SQLiteKeyValueStore::with_options(
            before_binding,
            VersionedSessionOptions {
                retained_bytes: 1234
            }
        ),
        Err(SQLiteError::SessionOptionsMismatch)
    ));
}

#[test]
fn binding_during_a_native_transaction_fails_before_mutating_either_format() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    connection.begin_transaction().unwrap();
    connection.with(|sqlite| {
        sqlite.execute_batch("CREATE TABLE _key_value (key BLOB PRIMARY KEY, value BLOB NOT NULL) WITHOUT ROWID; INSERT INTO _key_value VALUES (x'61', x'62')")?;
        Ok(())
    }).unwrap();
    assert!(matches!(
        SQLiteKeyValueStore::new(connection.clone()),
        Err(SQLiteError::TransactionAlreadyActive)
    ));
    connection
        .with(|sqlite| {
            assert_eq!(
                sqlite.query_row(
                    "SELECT value FROM _key_value WHERE key = x'61'",
                    [],
                    |row| row.get::<_, Vec<u8>>(0)
                )?,
                b"b"
            );
            assert_eq!(
                sqlite.query_row(
                    "SELECT count(*) FROM sqlite_schema WHERE name LIKE '_uqa_mvcc_%'",
                    [],
                    |row| row.get::<_, i64>(0)
                )?,
                0
            );
            Ok(())
        })
        .unwrap();
    connection.commit_transaction().unwrap();
    let store = SQLiteKeyValueStore::new(connection).unwrap();
    assert_eq!(store.get(b"a").unwrap().as_deref(), Some(b"b".as_slice()));
}

#[test]
fn legacy_bytes_migrate_atomically_and_the_old_writable_name_becomes_a_guard() {
    for mode in MODES {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("legacy.db");
        let connection = open(mode, &path);
        connection.with(|sqlite| {
            sqlite.execute_batch("CREATE TABLE _key_value (key BLOB PRIMARY KEY, value BLOB NOT NULL) WITHOUT ROWID; INSERT INTO _key_value VALUES (x'', x'00ff'), (x'61', x'6265666f7265'), (x'ff00', x'01')")?;
            Ok(())
        }).unwrap();
        let store = SQLiteKeyValueStore::new(connection.clone()).unwrap();
        assert_eq!(
            store.scan_prefix(b"").unwrap(),
            vec![
                (vec![], vec![0, 255]),
                (b"a".to_vec(), b"before".to_vec()),
                (vec![255, 0], vec![1])
            ]
        );
        connection
            .with_physical(|sqlite| {
                for sql in [
                    "INSERT INTO _key_value (key, value) VALUES (x'62', x'01')",
                    "UPDATE _key_value SET value = x'01'",
                    "DELETE FROM _key_value",
                ] {
                    assert!(
                        sqlite.execute_batch(sql).is_err(),
                        "legacy writer accepted: {sql}"
                    );
                }
                Ok(())
            })
            .unwrap();
        store.put(b"a", b"after").unwrap();
        drop(store);
        drop(connection);
        let reopened = SQLiteKeyValueStore::new(open(mode, &path)).unwrap();
        assert_eq!(
            reopened.get(b"a").unwrap().as_deref(),
            Some(b"after".as_slice())
        );
        assert_eq!(reopened.get(&[255, 0]).unwrap(), Some(vec![1]));
        assert!(matches!(
            Catalog::open(open(mode, &path)),
            Err(SQLiteError::LogicalSessionRequired)
        ));
    }
}

#[test]
fn native_or_unrelated_tables_are_not_adopted_as_a_key_value_database() {
    for native in [true, false] {
        let connection = ManagedConnection::open_in_memory().unwrap();
        if native {
            Catalog::open(connection.clone()).unwrap();
        } else {
            connection.with(|sqlite| {
                sqlite.execute_batch("CREATE TABLE sqliteXrecords (payload INTEGER NOT NULL); INSERT INTO sqliteXrecords VALUES (7)")?;
                Ok(())
            }).unwrap();
        }
        let schema = || {
            connection
                .with(|sqlite| {
                    Ok(sqlite.query_row(
                        "SELECT group_concat(sql, ';') FROM sqlite_schema",
                        [],
                        |row| row.get::<_, String>(0),
                    )?)
                })
                .unwrap()
        };
        let before = schema();
        assert!(SQLiteKeyValueStore::new(connection.clone()).is_err());
        assert_eq!(schema(), before);
        if !native {
            connection
                .with(|sqlite| {
                    assert_eq!(
                        sqlite.query_row("SELECT payload FROM sqliteXrecords", [], |row| row
                            .get::<_, i64>(0))?,
                        7
                    );
                    Ok(())
                })
                .unwrap();
        }
    }
}

#[test]
fn migration_exhaustion_restores_the_complete_legacy_format_before_retry() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    connection.with(|sqlite| {
        sqlite.execute_batch("CREATE TABLE _key_value (key BLOB PRIMARY KEY, value BLOB NOT NULL) WITHOUT ROWID; INSERT INTO _key_value VALUES (x'61', zeroblob(4096)), (x'62', x'01')")?;
        Ok(())
    }).unwrap();
    assert!(matches!(
        SQLiteKeyValueStore::with_options(
            connection.clone(),
            VersionedSessionOptions {
                retained_bytes: 1024
            }
        ),
        Err(SQLiteError::Memory(_))
    ));
    connection
        .with(|sqlite| {
            assert_eq!(
                sqlite.query_row("SELECT count(*) FROM _key_value", [], |row| row
                    .get::<_, i64>(0))?,
                2
            );
            assert_eq!(
                sqlite.query_row(
                    "SELECT count(*) FROM sqlite_schema WHERE name LIKE '_uqa_mvcc_%'",
                    [],
                    |row| row.get::<_, i64>(0)
                )?,
                0
            );
            Ok(())
        })
        .unwrap();
    let store = SQLiteKeyValueStore::new(connection).unwrap();
    assert_eq!(store.get(b"a").unwrap().unwrap().len(), 4096);
    assert_eq!(store.get(b"b").unwrap(), Some(vec![1]));
}

#[test]
fn key_only_reads_preserve_private_tombstones_without_fetching_large_values() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("keys.db");
    {
        let store = SQLiteKeyValueStore::open(&path).unwrap();
        store.put(b"a", &vec![1; 65536]).unwrap();
        store.put(b"b", &vec![2; 65536]).unwrap();
    }
    let storage = SQLiteKeyValueStorage::open_with_options(
        &path,
        VersionedSessionOptions {
            retained_bytes: 4096,
        },
    )
    .unwrap();
    let store = storage.store().new_session();
    assert!(matches!(
        store.get(b"a"),
        Err(StorageBackendError::Memory(_))
    ));
    assert!(store.contains_key(b"a").unwrap());
    assert_eq!(
        store.scan_prefix_keys_after(b"", None, 2).unwrap(),
        vec![b"a".to_vec(), b"b".to_vec()]
    );
    let control = StorageReadControl::with_limit(32);
    assert!(store.contains_prefix_budgeted(b"a", &control).unwrap());
    store.begin_transaction().unwrap();
    store.delete(b"a").unwrap();
    store.put(b"c", b"private").unwrap();
    assert!(!store.contains_prefix_budgeted(b"a", &control).unwrap());
    assert!(store.contains_prefix_budgeted(b"c", &control).unwrap());
    assert_eq!(
        store.scan_prefix_keys_after(b"", None, 2).unwrap(),
        vec![b"b".to_vec(), b"c".to_vec()]
    );
    store.rollback_transaction().unwrap();
    assert!(store.contains_prefix_budgeted(b"a", &control).unwrap());
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn migration_collision_rolls_back_earlier_copies_and_preserves_both_input_formats() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    connection.with(|sqlite| {
        sqlite.execute_batch("CREATE TABLE _key_value (key BLOB PRIMARY KEY, value BLOB NOT NULL) WITHOUT ROWID; INSERT INTO _key_value VALUES (x'30', x'6669727374'), (x'61', x'6c6567616379')")?;
        Ok(())
    }).unwrap();
    let records = uqa_storage_sqlite::SQLiteRecordStore::new(&connection).unwrap();
    let existing = super::session(&records);
    existing.put(b"a", b"record").unwrap();
    let before = existing.change_version().unwrap();
    assert!(SQLiteKeyValueStore::new(connection.clone()).is_err());
    assert_eq!(existing.get(b"0").unwrap(), None);
    assert_eq!(
        existing.get(b"a").unwrap().as_deref(),
        Some(b"record".as_slice())
    );
    assert_eq!(existing.change_version().unwrap(), before);
    connection
        .with(|sqlite| {
            assert_eq!(
                sqlite.query_row("SELECT count(*) FROM _key_value", [], |row| row
                    .get::<_, i64>(0))?,
                2
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn orphaned_or_changed_legacy_guards_are_rejected_without_recreating_records() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    connection
        .with(|sqlite| {
            sqlite
                .execute_batch("CREATE VIEW _key_value AS SELECT 1 AS versioned_storage_format")?;
            Ok(())
        })
        .unwrap();
    assert!(SQLiteKeyValueStore::new(connection.clone()).is_err());
    connection
        .with_physical(|sqlite| {
            assert_eq!(
                sqlite.query_row(
                    "SELECT count(*) FROM sqlite_schema WHERE name LIKE '_uqa_mvcc_%'",
                    [],
                    |row| row.get::<_, i64>(0)
                )?,
                0
            );
            sqlite.execute_batch("DROP VIEW _key_value")?;
            Ok(())
        })
        .unwrap();
    for alteration in [
        "DROP VIEW _key_value",
        "DROP VIEW _key_value; CREATE VIEW _key_value AS SELECT 2 AS versioned_storage_format",
        "DROP VIEW _metadata",
        "DROP VIEW _metadata; CREATE VIEW _metadata AS SELECT 'storage_kind' AS key, 'native' AS value",
    ] {
        let connection = ManagedConnection::open_in_memory().unwrap();
        let independent = connection.new_session();
        let store = SQLiteKeyValueStore::new(connection.clone()).unwrap();
        store.put(b"a", b"retained").unwrap();
        let definitions = connection.with_physical(|sqlite| {
            sqlite.execute_batch(alteration)?;
            Ok(sqlite.query_row("SELECT group_concat(sql, ';') FROM sqlite_schema WHERE name IN ('_metadata', '_key_value')", [], |row| row.get::<_, Option<String>>(0))?)
        }).unwrap();
        assert!(SQLiteKeyValueStore::new(independent).is_err(), "{alteration}");
        connection.with_physical(|sqlite| {
            assert_eq!(sqlite.query_row("SELECT group_concat(sql, ';') FROM sqlite_schema WHERE name IN ('_metadata', '_key_value')", [], |row| row.get::<_, Option<String>>(0))?, definitions);
            Ok(())
        }).unwrap();
        assert_eq!(
            store.get(b"a").unwrap().as_deref(),
            Some(b"retained".as_slice())
        );
    }
}
