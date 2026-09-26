//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_storage::mvcc::{IdentifierRequest, VersionedPersistence};

use super::*;
use crate::mvcc::native::tests::{
    diskann::remove_empty_table, materialization::with, namespace_upgrade::preserved,
};

fn predecessor(connection: &ManagedConnection) {
    with(connection, |sqlite| {
        let _permit = schema::WritePermit::acquire(sqlite)?;
        let data = namespace(sqlite)?.0;
        let transaction = schema::begin(sqlite)?;
        remove_empty_table(&transaction)?;
        transaction.execute_batch("DROP TABLE _uqa_mvcc_native_format")?;
        transaction.execute_batch(FORMAT_NINE)?;
        transaction.execute(
            "INSERT INTO _uqa_mvcc_native_format VALUES (1,9,49,?1)",
            [data.as_bytes().as_slice()],
        )?;
        for action in ["INSERT", "UPDATE", "DELETE"] {
            transaction.execute_batch(&schema::trigger(TABLES[0].0, action).1)?;
        }
        transaction.commit()?;
        validate_format(sqlite, 9)?;
        Ok(())
    });
}

#[test]
fn native_diskann_upgrade_preserves_history_namespace_and_failed_upgrade_atomicity() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let control = StorageReadControl::with_limit(1 << 22);
    let records = SQLiteRecordStore::for_native(&connection, &control).unwrap();
    let data = records.native_namespace().unwrap();
    let pending = records.allocate_transaction(&control).unwrap();
    records
        .allocate_identifiers(
            b"diskann-preserved",
            IdentifierRequest::Observe(77),
            &control,
        )
        .unwrap();
    predecessor(&connection);
    let before = preserved(&connection);
    with(&connection, |sqlite| {
        let _permit = schema::WritePermit::acquire(sqlite)?;
        let transaction = schema::begin(sqlite)?;
        let upgraded = initialize_in(&transaction, &control)?;
        assert_eq!(upgraded.namespace.0, data);
        check_mapping_version(&transaction, CURRENT_VERSION)?;
        assert!(check_mapping_version(&transaction, 9).is_err());
        drop(transaction);
        validate_format(sqlite, 9)?;
        check_mapping_version(sqlite, 9)?;
        assert!(sqlite
            .prepare("SELECT * FROM _uqa_mvcc_native_diskann_records")
            .is_err());
        Ok(())
    });
    assert_eq!(preserved(&connection), before);
    let upgraded = SQLiteRecordStore::for_native(&connection, &control).unwrap();
    assert_eq!(preserved(&connection), before);
    assert_eq!(upgraded.native_namespace(), Some(data));
    assert_eq!(
        upgraded.commit_status(pending, &control).unwrap(),
        uqa_storage::mvcc::CommitStatus::Pending
    );
    assert_eq!(
        upgraded
            .identifier_watermark(b"diskann-preserved", &control)
            .unwrap(),
        Some(77)
    );
    with(&connection, |sqlite| {
        validate_format(sqlite, CURRENT_VERSION)?;
        assert!(check_mapping_version(sqlite, 9).is_err());
        Ok(())
    });
}

#[test]
fn native_diskann_upgrade_keeps_data_namespace_independent_from_history() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let control = StorageReadControl::with_limit(1 << 22);
    let records = SQLiteRecordStore::for_native(&connection, &control).unwrap();
    let data = records.native_namespace().unwrap();
    drop(records);
    predecessor(&connection);
    let history = DatabaseId::from_bytes([219; 16]);
    assert_ne!(data, history);
    with(&connection, |sqlite| {
        let _permit = schema::WritePermit::acquire(sqlite)?;
        sqlite.execute(
            "UPDATE _uqa_mvcc_metadata SET database_id=?1 WHERE singleton=1",
            [history.as_bytes().as_slice()],
        )?;
        Ok(())
    });
    let before = preserved(&connection);
    let upgraded = SQLiteRecordStore::for_native(&connection, &control).unwrap();
    assert_eq!(upgraded.database_id(), history);
    assert_eq!(upgraded.native_namespace(), Some(data));
    assert_eq!(preserved(&connection), before);
}

#[test]
fn native_diskann_reopen_rejects_missing_tables_guards_and_changed_layouts() {
    for fault in [
        "DROP TABLE _uqa_mvcc_native_diskann_records",
        "DROP TRIGGER _uqa_mvcc_native_capture_57_UPDATE",
        "ALTER TABLE _uqa_mvcc_native_diskann_records ADD COLUMN unexpected BLOB",
        "DROP TABLE _uqa_mvcc_native_vector_origins",
        "DROP TRIGGER _uqa_mvcc_native_capture_58_UPDATE",
        "ALTER TABLE _uqa_mvcc_native_vector_origins ADD COLUMN unexpected BLOB",
        "DROP TABLE _uqa_mvcc_native_vector_changes",
        "DROP TRIGGER _uqa_mvcc_native_capture_59_UPDATE",
        "ALTER TABLE _uqa_mvcc_native_vector_changes ADD COLUMN unexpected BLOB",
    ] {
        let connection = ManagedConnection::open_in_memory().unwrap();
        let control = StorageReadControl::with_limit(1 << 22);
        SQLiteRecordStore::for_native(&connection, &control).unwrap();
        with(&connection, |sqlite| {
            sqlite.execute_batch(fault)?;
            Ok(())
        });
        let schema = with(&connection, |sqlite| {
            Ok(sqlite
                .prepare("SELECT type,name,sql FROM sqlite_schema ORDER BY type,name")?
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?)
        });
        assert!(
            SQLiteRecordStore::for_native(&connection, &control).is_err(),
            "{fault}"
        );
        let after = with(&connection, |sqlite| {
            Ok(sqlite
                .prepare("SELECT type,name,sql FROM sqlite_schema ORDER BY type,name")?
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?)
        });
        assert_eq!(after, schema, "{fault}");
    }
}

#[test]
fn native_canonical_origin_upgrade_from_ten_is_atomic_and_preserves_existing_history() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let control = StorageReadControl::with_limit(1 << 22);
    let records = SQLiteRecordStore::for_native(&connection, &control).unwrap();
    let data = records.native_namespace().unwrap();
    let pending = records.allocate_transaction(&control).unwrap();
    with(&connection, |sqlite| {
        let _permit = schema::WritePermit::acquire(sqlite)?;
        let transaction = schema::begin(sqlite)?;
        crate::mvcc::native::tests::diskann::remove_empty_origins(&transaction)?;
        transaction.execute_batch("DROP TABLE _uqa_mvcc_native_format")?;
        transaction.execute_batch(FORMAT_TEN)?;
        transaction.execute(
            "INSERT INTO _uqa_mvcc_native_format VALUES (1,10,49,?1)",
            [data.as_bytes().as_slice()],
        )?;
        for action in ["INSERT", "UPDATE", "DELETE"] {
            transaction.execute_batch(&schema::trigger(TABLES[0].0, action).1)?;
        }
        transaction.commit()?;
        validate_format(sqlite, 10)?;
        Ok(())
    });
    let before = preserved(&connection);
    with(&connection, |sqlite| {
        let _permit = schema::WritePermit::acquire(sqlite)?;
        let transaction = schema::begin(sqlite)?;
        let mapping = initialize_in(&transaction, &control)?;
        assert_eq!(mapping.namespace.0, data);
        assert!(check_mapping_version(&transaction, 10).is_err());
        drop(transaction);
        validate_format(sqlite, 10)?;
        assert!(sqlite
            .prepare("SELECT * FROM _uqa_mvcc_native_vector_origins")
            .is_err());
        Ok(())
    });
    assert_eq!(preserved(&connection), before);
    let upgraded = SQLiteRecordStore::for_native(&connection, &control).unwrap();
    assert_eq!(upgraded.native_namespace(), Some(data));
    assert_eq!(
        upgraded.commit_status(pending, &control).unwrap(),
        uqa_storage::mvcc::CommitStatus::Pending
    );
    assert_eq!(preserved(&connection), before);
    with(&connection, |sqlite| {
        validate_format(sqlite, CURRENT_VERSION)?;
        assert!(check_mapping_version(sqlite, 10).is_err());
        Ok(())
    });
}
