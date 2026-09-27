//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::mvcc::native::tests::{
    diskann::remove_empty_populations, materialization::with, namespace_upgrade::preserved,
};
use uqa_storage::mvcc::{CommitStatus, PreparedRecordCommit, VersionedPersistence};

#[test]
fn native_diskann_population_format_upgrade_is_atomic_and_preserves_history_and_receipts() {
    let connection = ManagedConnection::open_in_memory().unwrap();
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
        remove_empty_populations(&transaction)?;
        transaction.execute_batch("DROP TABLE _uqa_mvcc_native_format")?;
        transaction.execute_batch(FORMAT_TWELVE)?;
        transaction.execute(
            "INSERT INTO _uqa_mvcc_native_format VALUES(1,12,49,?1)",
            [namespace.as_bytes().as_slice()],
        )?;
        for action in ["INSERT", "UPDATE", "DELETE"] {
            transaction.execute_batch(&schema::trigger(TABLES[0].0, action).1)?;
        }
        transaction.commit()?;
        validate_format(sqlite, 12)?;
        Ok(())
    });
    let before = preserved(&connection);
    with(&connection, |sqlite| {
        let _permit = schema::WritePermit::acquire(sqlite)?;
        let transaction = schema::begin(sqlite)?;
        let upgraded = initialize_in(&transaction, &control)?;
        assert_eq!(upgraded.namespace.0, namespace);
        check_mapping_version(&transaction, CURRENT_VERSION)?;
        assert!(check_mapping_version(&transaction, 12).is_err());
        drop(transaction);
        validate_format(sqlite, 12)?;
        for (family, _) in super::super::super::populations::schema::TABLES {
            assert!(sqlite
                .prepare(&format!("SELECT * FROM {}", family.layout().table))
                .is_err());
        }
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
        assert!(check_mapping_version(sqlite, 12).is_err());
        Ok(())
    });
}
