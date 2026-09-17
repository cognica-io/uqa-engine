//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Adding native IVF guards preserves old data, identities and transaction receipts atomically.

use super::{
    materialization::{initialize, with},
    persistence::connection,
    *,
};
use crate::{mvcc::schema, SQLiteRecordStore};
use rusqlite::types::Value;
use uqa_storage::mvcc::{CommitStatus, PreparedRecordCommit, VersionedPersistence};

const PREVIOUS: &str = "CREATE TABLE _uqa_mvcc_native_format (singleton INTEGER PRIMARY KEY CHECK(singleton = 1), format INTEGER NOT NULL CHECK(format = 5), catalog_version INTEGER NOT NULL CHECK(catalog_version = 49))";

fn downgrade(connection: &ManagedConnection) {
    with(connection, |sqlite| {
        let _permit = schema::WritePermit::acquire(sqlite)?;
        let transaction = schema::begin(sqlite)?;
        transaction.execute_batch(
            "DROP TABLE _uqa_mvcc_native_ivf_guards; DROP TABLE _uqa_mvcc_native_format",
        )?;
        transaction.execute_batch(PREVIOUS)?;
        transaction.execute("INSERT INTO _uqa_mvcc_native_format VALUES (1, 5, 49)", [])?;
        for action in ["INSERT", "UPDATE", "DELETE"] {
            transaction.execute_batch(&schema::trigger("_uqa_mvcc_native_format", action).1)?;
        }
        transaction.commit()?;
        Ok(())
    });
}

fn history(connection: &ManagedConnection) -> Vec<Vec<Vec<Value>>> {
    with(connection, |sqlite| {
        let mut tables = Vec::new();
        for query in [
            "SELECT * FROM _uqa_mvcc_metadata ORDER BY singleton",
            "SELECT * FROM _uqa_mvcc_heads ORDER BY key",
            "SELECT * FROM _uqa_mvcc_versions ORDER BY key,sequence",
            "SELECT * FROM _uqa_mvcc_transactions ORDER BY allocation",
        ] {
            let mut statement = sqlite.prepare(query)?;
            let columns = statement.column_count();
            tables.push(
                statement
                    .query_map([], |row| {
                        (0..columns)
                            .map(|column| row.get(column))
                            .collect::<rusqlite::Result<Vec<_>>>()
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?,
            );
        }
        Ok(tables)
    })
}

#[test]
fn native_ivf_guard_upgrade_preserves_closed_files_and_receipts() {
    for mode in 0..4 {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("upgrade.db");
        let original = connection(&path, mode);
        initialize(&original);
        let control = StorageReadControl::with_limit(1 << 24);
        let store = SQLiteRecordStore::for_native(&original, &control).unwrap();
        let identity = store.database_id();
        let allocation = store.allocate_transaction(&control).unwrap();
        let receipt = store
            .commit(
                allocation,
                &PreparedRecordCommit::new(&[], &control).unwrap(),
                &control,
            )
            .unwrap();
        let pending = store.allocate_transaction(&control).unwrap();
        let before = history(&original);
        downgrade(&original);
        drop((store, original));
        let reopened = connection(&path, mode);
        let upgraded = SQLiteRecordStore::for_native(&reopened, &control).unwrap();
        assert_eq!(upgraded.database_id(), identity);
        assert_eq!(history(&reopened), before);
        assert_eq!(
            upgraded.commit_status(allocation, &control).unwrap(),
            CommitStatus::Committed(receipt)
        );
        assert_eq!(
            upgraded.commit_status(pending, &control).unwrap(),
            CommitStatus::Pending
        );
        with(&reopened, |sqlite| {
            assert_eq!(
                sqlite.query_row("SELECT format FROM _uqa_mvcc_native_format", [], |row| row
                    .get::<_, i64>(
                    0
                ))?,
                6
            );
            assert_eq!(
                sqlite.query_row(
                    "SELECT count(*) FROM _uqa_mvcc_native_ivf_guards",
                    [],
                    |row| row.get::<_, i64>(0)
                )?,
                0
            );
            Ok(())
        });
        drop(upgraded);
        let again = SQLiteRecordStore::for_native(&reopened, &control).unwrap();
        assert_eq!(again.database_id(), identity);
        assert_eq!(history(&reopened), before);
    }
}

#[test]
fn failed_native_ivf_guard_upgrade_rolls_back_its_new_table() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    initialize(&connection);
    let control = StorageReadControl::with_limit(1 << 24);
    let store = SQLiteRecordStore::for_native(&connection, &control).unwrap();
    let before = history(&connection);
    downgrade(&connection);
    with(&connection, |sqlite| {
        sqlite.execute_batch("CREATE TRIGGER _uqa_mvcc_native_ivf_guards_INSERT_guard BEFORE INSERT ON _metadata BEGIN SELECT 1; END")?;
        Ok(())
    });
    assert!(SQLiteRecordStore::for_native(&connection, &control).is_err());
    assert_eq!(history(&connection), before);
    with(&connection, |sqlite| {
        assert_eq!(
            sqlite.query_row("SELECT format FROM _uqa_mvcc_native_format", [], |row| row
                .get::<_, i64>(
                0
            ))?,
            5
        );
        assert_eq!(
            sqlite.query_row(
                "SELECT count(*) FROM sqlite_schema WHERE name='_uqa_mvcc_native_ivf_guards'",
                [],
                |row| row.get::<_, i64>(0)
            )?,
            0
        );
        sqlite.execute_batch("DROP TRIGGER _uqa_mvcc_native_ivf_guards_INSERT_guard")?;
        Ok(())
    });
    let repaired = SQLiteRecordStore::for_native(&connection, &control).unwrap();
    assert_eq!(repaired.database_id(), store.database_id());
    assert_eq!(history(&connection), before);
}
