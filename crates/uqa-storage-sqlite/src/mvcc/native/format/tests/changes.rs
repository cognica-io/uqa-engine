//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::mvcc::native::tests::{
    diskann::remove_empty_changes, materialization::with, namespace_upgrade::preserved,
};
use rusqlite::types::ValueRef;
use uqa_storage::{
    diskann_index::format::{DiskANNCanonicalOrigin, DiskANNVectorVersion},
    mvcc::{VersionedPersistence, VersionedSessionOptions},
};

#[test]
fn native_diskann_change_upgrade_preserves_origin_only_history_and_rolls_back_schema() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let control = StorageReadControl::with_limit(1 << 22);
    connection
        .bind_native_records(VersionedSessionOptions::default())
        .unwrap();
    let version = origin_only(&connection);
    let records = SQLiteRecordStore::for_native(&connection, &control).unwrap();
    let namespace = records.native_namespace().unwrap();
    let pending = records.allocate_transaction(&control).unwrap();
    with(&connection, |sqlite| {
        let _permit = schema::WritePermit::acquire(sqlite)?;
        let transaction = schema::begin(sqlite)?;
        remove_empty_changes(&transaction)?;
        transaction.execute_batch("DROP TABLE _uqa_mvcc_native_format")?;
        transaction.execute_batch(FORMAT_ELEVEN)?;
        transaction.execute(
            "INSERT INTO _uqa_mvcc_native_format VALUES(1,11,49,?1)",
            [namespace.as_bytes().as_slice()],
        )?;
        for action in ["INSERT", "UPDATE", "DELETE"] {
            transaction.execute_batch(&schema::trigger(TABLES[0].0, action).1)?;
        }
        transaction.commit()?;
        validate_format(sqlite, 11)?;
        Ok(())
    });
    let before = preserved(&connection);
    with(&connection, |sqlite| {
        let _permit = schema::WritePermit::acquire(sqlite)?;
        let transaction = schema::begin(sqlite)?;
        let upgraded = initialize_in(&transaction, &control)?;
        assert_eq!(upgraded.namespace.0, namespace);
        check_mapping_version(&transaction, CURRENT_VERSION)?;
        assert!(check_mapping_version(&transaction, 11).is_err());
        drop(transaction);
        validate_format(sqlite, 11)?;
        assert!(sqlite
            .prepare("SELECT * FROM _uqa_mvcc_native_vector_changes")
            .is_err());
        Ok(())
    });
    assert_eq!(preserved(&connection), before);
    let upgraded = SQLiteRecordStore::for_native(&connection, &control).unwrap();
    assert_eq!(upgraded.native_namespace(), Some(namespace));
    assert_eq!(preserved(&connection), before);
    assert_eq!(
        upgraded.commit_status(pending, &control).unwrap(),
        uqa_storage::mvcc::CommitStatus::Pending
    );
    with(&connection, |sqlite| {
        validate_format(sqlite, CURRENT_VERSION)?;
        assert!(check_mapping_version(sqlite, 11).is_err());
        Ok(())
    });
    let source = crate::SQLiteDiskANNCanonical::new(connection, "docs", "embedding", 2)
        .unwrap()
        .retain(&control)
        .unwrap();
    assert_eq!(source.origin(1, &control).unwrap(), Some(version));
    assert!(source.next_change_after(None, &control).unwrap().is_none());
    let mut seen = 0;
    source
        .visit_document(1, &control, &mut |ordinal, actual, raw| {
            assert_eq!((ordinal, actual), (0, version));
            assert_eq!(raw, [1.0, 0.0]);
            seen += 1;
            Ok(())
        })
        .unwrap();
    assert_eq!(seen, 1);
}

fn origin_only(connection: &ManagedConnection) -> DiskANNVectorVersion {
    // A predecessor writer publishes raw coordinates and an origin without the later journal family.
    connection
        .with_native_versioned_write(|origin, snapshot, batch| {
            let owner = snapshot.ensure_table_owner("docs", batch)?;
            let version = DiskANNVectorVersion::new(origin.transaction(), origin.revision())?;
            snapshot.put_row(
                batch,
                Family::Vectors,
                owner,
                &[
                    ValueRef::Text(b"docs"),
                    ValueRef::Text(b"embedding"),
                    ValueRef::Integer(1),
                    ValueRef::Integer(0),
                    ValueRef::Blob(&[0, 0, 128, 63, 0, 0, 0, 0]),
                ],
            )?;
            snapshot.put_row(
                batch,
                Family::VectorOrigins,
                owner,
                &[
                    ValueRef::Text(b"docs"),
                    ValueRef::Text(b"embedding"),
                    ValueRef::Integer(1),
                    ValueRef::Blob(&DiskANNCanonicalOrigin::new(version, 2, 1)?.encode()),
                ],
            )?;
            Ok(version)
        })
        .unwrap()
}
