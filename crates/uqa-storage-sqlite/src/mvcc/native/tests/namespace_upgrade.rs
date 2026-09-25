//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_storage::mvcc::{
    CommitStatus, IdentifierRequest, PreparedRecordCommit, VersionedPersistence,
};

use super::{materialization::with, persistence::connection};
use crate::mvcc::{native::format::initialize_in, schema};
use crate::{ManagedConnection, SQLiteRecordStore};
use uqa_storage::read_control::StorageReadControl;

const PREDECESSOR: &str = "CREATE TABLE _uqa_mvcc_native_format (singleton INTEGER PRIMARY KEY CHECK(singleton = 1), format INTEGER NOT NULL CHECK(format = 8), catalog_version INTEGER NOT NULL CHECK(catalog_version = 49))";

fn mapping_version(connection: &rusqlite::Connection) -> i64 {
    connection
        .query_row("SELECT format FROM _uqa_mvcc_native_format", [], |row| {
            row.get(0)
        })
        .unwrap()
}

fn install_predecessor(connection: &ManagedConnection) {
    with(connection, |sqlite| {
        let _permit = schema::WritePermit::acquire(sqlite)?;
        let transaction = schema::begin(sqlite)?;
        super::diskann::remove_empty_table(&transaction)?;
        transaction.execute_batch("DROP TABLE _uqa_mvcc_native_format")?;
        transaction.execute_batch(PREDECESSOR)?;
        transaction.execute("INSERT INTO _uqa_mvcc_native_format VALUES (1,8,49)", [])?;
        for action in ["INSERT", "UPDATE", "DELETE"] {
            transaction.execute_batch(&schema::trigger("_uqa_mvcc_native_format", action).1)?;
        }
        transaction.commit()?;
        assert_eq!(mapping_version(sqlite), 8);
        Ok(())
    });
}

pub(in crate::mvcc::native) fn preserved(
    connection: &ManagedConnection,
) -> Vec<Vec<Vec<rusqlite::types::Value>>> {
    with(connection, |sqlite| {
        [
            "_uqa_mvcc_metadata",
            "_uqa_mvcc_heads",
            "_uqa_mvcc_runs",
            "_uqa_mvcc_versions",
            "_uqa_mvcc_transactions",
            "_uqa_mvcc_identifiers",
            "_uqa_mvcc_native_owners",
            "_metadata",
            "_documents",
        ]
        .iter()
        .map(|table| {
            let mut statement = sqlite.prepare(&format!("SELECT * FROM {table} ORDER BY 1,2"))?;
            let columns = statement.column_count();
            let rows = statement.query_map([], |row| {
                (0..columns).map(|column| row.get(column)).collect()
            })?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
        .collect()
    })
}

#[test]
fn namespace_upgrade_preserves_record_history_receipts_and_watermarks() {
    let directory = tempfile::tempdir().unwrap();
    let control = StorageReadControl::with_limit(1 << 24);
    for mode in 0..4 {
        let connection = connection(&directory.path().join(format!("upgrade-{mode}.db")), mode);
        let records = SQLiteRecordStore::for_native(&connection, &control).unwrap();
        let identity = records.database_id();
        let committed = records.allocate_transaction(&control).unwrap();
        let receipt = records
            .commit(
                committed,
                &PreparedRecordCommit::new(&[], &control).unwrap(),
                &control,
            )
            .unwrap();
        let pending = records.allocate_transaction(&control).unwrap();
        let aborted = records.allocate_transaction(&control).unwrap();
        records.abort(aborted, &control).unwrap();
        records
            .allocate_identifiers(b"stable", IdentifierRequest::Observe(77), &control)
            .unwrap();
        install_predecessor(&connection);
        let before = preserved(&connection);
        let upgraded = SQLiteRecordStore::for_native(&connection, &control).unwrap();
        assert_eq!(upgraded.database_id(), identity);
        assert_eq!(upgraded.native_namespace(), Some(identity));
        assert_eq!(preserved(&connection), before, "mode {mode}");
        assert_eq!(
            upgraded.commit_status(committed, &control).unwrap(),
            CommitStatus::Committed(receipt)
        );
        assert_eq!(
            upgraded.commit_status(pending, &control).unwrap(),
            CommitStatus::Pending
        );
        assert_eq!(
            upgraded.commit_status(aborted, &control).unwrap(),
            CommitStatus::Aborted
        );
        assert_eq!(
            upgraded.identifier_watermark(b"stable", &control).unwrap(),
            Some(77)
        );
        assert_eq!(
            upgraded
                .allocate_transaction(&control)
                .unwrap()
                .allocation(),
            aborted.allocation() + 1
        );
        with(&connection, |sqlite| {
            assert_eq!(mapping_version(sqlite), 10);
            Ok(())
        });
    }
}

#[test]
fn failed_namespace_upgrade_rolls_back_its_marker_and_guards() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let control = StorageReadControl::with_limit(1 << 24);
    let records = SQLiteRecordStore::for_native(&connection, &control).unwrap();
    let identity = records.database_id();
    install_predecessor(&connection);
    let before = preserved(&connection);
    with(&connection, |sqlite| {
        let _permit = schema::WritePermit::acquire(sqlite)?;
        let transaction = schema::begin(sqlite)?;
        let restored = initialize_in(&transaction, &control)?;
        assert_eq!(restored.identity, identity);
        assert_eq!(restored.namespace.0, identity);
        assert_eq!(mapping_version(&transaction), 10);
        drop(transaction);
        assert_eq!(mapping_version(sqlite), 8);
        assert!(sqlite
            .prepare("SELECT record_namespace FROM _uqa_mvcc_native_format")
            .is_err());
        Ok(())
    });
    assert_eq!(preserved(&connection), before);
    assert_eq!(
        SQLiteRecordStore::for_native(&connection, &control)
            .unwrap()
            .native_namespace(),
        Some(identity)
    );
    assert_eq!(preserved(&connection), before);
}
