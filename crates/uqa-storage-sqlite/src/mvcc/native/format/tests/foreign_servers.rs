//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::mvcc::native::tests::{
    diskann::remove_empty_foreign_server_metadata, materialization::with,
    namespace_upgrade::preserved,
};
use uqa_storage::mvcc::{CommitStatus, PreparedRecordCommit, VersionedPersistence};

#[test]
fn foreign_server_mapping_upgrade_preserves_history_and_rolls_back_atomically() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let catalog = Catalog::open(connection.clone()).unwrap();
    catalog
        .save_foreign_server("legacy", "memory", "original options")
        .unwrap();
    let control = StorageReadControl::with_limit(1 << 22);
    let records = SQLiteRecordStore::for_native(&connection, &control).unwrap();
    let namespace = records.native_namespace().unwrap();
    let committed = records.allocate_transaction(&control).unwrap();
    let receipt = records
        .commit(
            committed,
            &PreparedRecordCommit::new(&[], &control).unwrap(),
            &control,
        )
        .unwrap();
    let pending = records.allocate_transaction(&control).unwrap();
    with(&connection, |sqlite| {
        let _permit = schema::WritePermit::acquire(sqlite)?;
        let transaction = schema::begin(sqlite)?;
        remove_empty_foreign_server_metadata(&transaction)?;
        transaction.execute_batch("DROP TABLE _uqa_mvcc_native_format")?;
        transaction.execute_batch(FORMAT_FOURTEEN)?;
        transaction.execute(
            "INSERT INTO _uqa_mvcc_native_format VALUES(1,14,49,?1)",
            [namespace.as_bytes().as_slice()],
        )?;
        for action in ["INSERT", "UPDATE", "DELETE"] {
            transaction.execute_batch(&schema::trigger(TABLES[0].0, action).1)?;
        }
        transaction.commit()?;
        validate_format(sqlite, 14)?;
        Ok(())
    });
    let before = preserved(&connection);
    with(&connection, |sqlite| {
        let _permit = schema::WritePermit::acquire(sqlite)?;
        let transaction = schema::begin(sqlite)?;
        let upgraded = initialize_in(&transaction, &control)?;
        assert_eq!(upgraded.namespace.0, namespace);
        check_mapping_version(&transaction, CURRENT_VERSION)?;
        assert!(check_mapping_version(&transaction, 14).is_err());
        drop(transaction);
        validate_format(sqlite, 14)?;
        assert!(sqlite
            .prepare("SELECT * FROM _foreign_server_metadata")
            .is_err());
        Ok(())
    });
    assert_eq!(preserved(&connection), before);
    let upgraded = SQLiteRecordStore::for_native(&connection, &control).unwrap();
    assert_eq!(upgraded.native_namespace(), Some(namespace));
    assert_eq!(preserved(&connection), before);
    assert_eq!(
        upgraded.commit_status(committed, &control).unwrap(),
        CommitStatus::Committed(receipt)
    );
    assert_eq!(
        upgraded.commit_status(pending, &control).unwrap(),
        CommitStatus::Pending
    );
    with(&connection, |sqlite| {
        validate_format(sqlite, CURRENT_VERSION)?;
        assert!(check_mapping_version(sqlite, 14).is_err());
        assert_eq!(
            sqlite.query_row("SELECT count(*) FROM _foreign_server_metadata", [], |row| {
                row.get::<_, i64>(0)
            })?,
            0
        );
        assert_eq!(
            sqlite.query_row(
                "SELECT options FROM _foreign_servers WHERE name='legacy'",
                [],
                |row| row.get::<_, String>(0)
            )?,
            "original options"
        );
        Ok(())
    });
}
