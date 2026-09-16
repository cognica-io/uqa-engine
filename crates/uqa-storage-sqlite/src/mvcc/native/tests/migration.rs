//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_storage::mvcc::{CommitSequence, VersionedPersistence};

use super::*;
use super::{
    materialization::{initialize, records, with},
    persistence::connection,
};
use crate::SQLiteRecordStore;

#[test]
fn native_baseline_imports_existing_rows_and_rejects_bypass_handles() {
    let directory = tempfile::tempdir().unwrap();
    for mode in 0..4 {
        let path = directory.path().join(format!("baseline-{mode}.db"));
        let connection = connection(&path, mode);
        initialize(&connection);
        with(&connection, |connection| {
            connection.execute_batch("INSERT INTO _documents(table_name, doc_id, body) VALUES ('docs', 1, '{}'); INSERT INTO _document_blobs VALUES ('public.docs', 1, 'binary', zeroblob(16384));")?;
            Ok(())
        });
        let raw = SQLiteRecordStore::new(&connection).unwrap();
        let control = StorageReadControl::with_limit(1 << 24);
        let raw_snapshot = raw.snapshot(&control).unwrap();
        let store = SQLiteRecordStore::for_native(&connection, &control).unwrap();
        assert_eq!(store.database_id(), raw.database_id());
        let snapshot = store.snapshot(&control).unwrap();
        assert_eq!(snapshot.sequence(), CommitSequence::from_u64(1));
        for family in NativeRecordFamily::all() {
            for record in records(&connection, &store, family, &control) {
                assert_eq!(
                    &***snapshot
                        .get(record.key(), &control)
                        .unwrap()
                        .unwrap()
                        .value()
                        .unwrap(),
                    record.row()
                );
            }
        }
        let owners = records(
            &connection,
            &store,
            NativeRecordFamily::TableOwners,
            &control,
        );
        assert_eq!(owners.len(), 2);
        let (_, first) = decode_record(owners[0].key(), owners[0].row(), &control).unwrap();
        let (_, second) = decode_record(owners[1].key(), owners[1].row(), &control).unwrap();
        assert_ne!(first[1], second[1]);
        assert!(raw.snapshot(&control).is_err());
        assert!(raw_snapshot.get(b"anything", &control).is_err());
        assert!(raw.allocate_transaction(&control).is_err());
        assert!(SQLiteRecordStore::new(&connection).is_err());
        assert!(Catalog::open(connection.clone()).is_err());
        assert!(connection.with(|_| Ok(())).is_err());
        assert!(connection
            .with_physical(|connection| {
                connection.execute("DELETE FROM _documents", [])?;
                Ok(())
            })
            .is_err());
        let reopened = SQLiteRecordStore::for_native(&connection, &control).unwrap();
        assert_eq!(reopened.database_id(), store.database_id());
        assert_eq!(
            reopened.snapshot(&control).unwrap().sequence(),
            CommitSequence::from_u64(1)
        );
    }
}

#[test]
fn failed_native_import_restores_the_original_schema_and_can_be_retried() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    initialize(&connection);
    with(&connection, |connection| {
        connection.execute(
            "INSERT INTO _document_blobs VALUES ('public.docs', 1, 'large', zeroblob(1048576))",
            [],
        )?;
        Ok(())
    });
    let small = StorageReadControl::with_limit(1 << 15);
    assert!(matches!(
        SQLiteRecordStore::for_native(&connection, &small),
        Err(VersionError::Memory(_))
    ));
    assert_eq!(small.memory().used(), 0);
    connection
        .with(|connection| {
            assert_eq!(
                connection.query_row(
                    "SELECT value FROM _metadata WHERE key='schema_version'",
                    [],
                    |row| row.get::<_, String>(0)
                )?,
                "48"
            );
            assert_eq!(
                connection.query_row(
                    "SELECT count(*) FROM sqlite_schema WHERE name GLOB '_uqa_mvcc_*'",
                    [],
                    |row| row.get::<_, i64>(0)
                )?,
                0
            );
            assert_eq!(
                connection.query_row(
                    "SELECT length(bytes) FROM _document_blobs WHERE field_name='large'",
                    [],
                    |row| row.get::<_, i64>(0)
                )?,
                1_048_576
            );
            Ok(())
        })
        .unwrap();
    SQLiteRecordStore::for_native(&connection, &StorageReadControl::with_limit(1 << 25)).unwrap();
}

#[test]
fn native_mapping_rejects_mixed_histories_and_incomplete_guards() {
    for change in [
        "DROP TRIGGER _documents_INSERT_guard",
        "DROP TRIGGER _uqa_mvcc_native_capture_10_UPDATE",
        "DROP TABLE _uqa_mvcc_native_changes",
        "DROP INDEX _uqa_mvcc_native_owner_identity",
    ] {
        let connection = ManagedConnection::open_in_memory().unwrap();
        initialize(&connection);
        let control = StorageReadControl::with_limit(1 << 24);
        SQLiteRecordStore::for_native(&connection, &control).unwrap();
        with(&connection, |connection| {
            connection.execute_batch(change)?;
            Ok(())
        });
        assert!(
            SQLiteRecordStore::for_native(&connection, &control).is_err(),
            "{change}"
        );
        assert!(SQLiteRecordStore::new(&connection).is_err(), "{change}");
    }
    let connection = ManagedConnection::open_in_memory().unwrap();
    initialize(&connection);
    let control = StorageReadControl::with_limit(1 << 24);
    let raw = SQLiteRecordStore::new(&connection).unwrap();
    raw.allocate_transaction(&control).unwrap();
    assert!(SQLiteRecordStore::for_native(&connection, &control).is_err());
    assert!(Catalog::open(connection).is_ok());
}
