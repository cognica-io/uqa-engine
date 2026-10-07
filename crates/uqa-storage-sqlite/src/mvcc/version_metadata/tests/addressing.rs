//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical addresses preserve logical uniqueness and survive compaction.

use super::*;

#[test]
fn physical_addresses_preserve_uniqueness_rollback_and_vacuum() {
    let connection = Connection::open_in_memory().unwrap();
    schema::initialize(&connection).unwrap();
    let _permit = schema::WritePermit::acquire(&connection).unwrap();
    connection
        .execute_batch(
            "INSERT INTO _uqa_mvcc_versions (key, sequence, value) VALUES
         (x'61', x'0000000000000001', x'01'),
         (x'62', x'0000000000000001', x'02'),
         (x'63', x'0000000000000001', x'03');
         DELETE FROM _uqa_mvcc_versions WHERE key = x'62';",
        )
        .unwrap();
    let addresses = || {
        connection
            .prepare("SELECT key, version_id FROM _uqa_mvcc_version_metadata ORDER BY key")
            .unwrap()
            .query_map([], |row| {
                Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, i64>(1)?))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    };
    let before = addresses();
    assert_eq!(before, [(vec![b'a'], 1), (vec![b'c'], 3)]);
    for sql in [
        "INSERT INTO _uqa_mvcc_versions (key, sequence, value) VALUES (x'61', x'0000000000000001', x'ff')",
        "UPDATE _uqa_mvcc_versions SET key = x'61' WHERE key = x'63'",
    ] {
        assert!(connection.execute_batch(sql).is_err());
        assert_eq!(addresses(), before);
    }
    connection.execute_batch("VACUUM").unwrap();
    assert_eq!(addresses(), before);
    let rows: Vec<(Vec<u8>, Vec<u8>)> = connection
        .prepare("SELECT m.key, v.value FROM _uqa_mvcc_version_metadata m JOIN _uqa_mvcc_versions v USING(version_id) ORDER BY m.key")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(rows, [(vec![b'a'], vec![1]), (vec![b'c'], vec![3])]);
    connection
        .execute_batch(
            "BEGIN;
         UPDATE _uqa_mvcc_versions SET version_id = 30 WHERE key = x'63';
         DELETE FROM _uqa_mvcc_versions WHERE key = x'61';
         ROLLBACK;",
        )
        .unwrap();
    assert_eq!(addresses(), before);
}

#[test]
fn predecessor_conversion_rolls_back_schema_and_copied_payloads_together() {
    use crate::{ManagedConnection, SQLiteRecordStore};
    use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
    use uqa_storage::mvcc::{CommitSequence, VersionedPersistence};

    let connection = ManagedConnection::open_in_memory().unwrap();
    let store = SQLiteRecordStore::new(&connection).unwrap();
    let control = uqa_storage::read_control::StorageReadControl::with_limit(1 << 20);
    let prepared = uqa_storage::mvcc::PreparedRecordCommit::new(
        &[uqa_storage::mvcc::RecordWrite {
            key: b"kept",
            expected: None,
            value: Some(b"original"),
        }],
        &control,
    )
    .unwrap();
    let receipt = store
        .commit(
            store.allocate_transaction(&control).unwrap(),
            &prepared,
            &control,
        )
        .unwrap();
    crate::mvcc::tests::downgrade_record_format(&store, 58);
    store
        .with(|sqlite| {
            // Fail after copying the old payloads, before the old table is removed.
            sqlite.authorizer(Some(|context: AuthContext<'_>| match context.action {
                AuthAction::DropTable {
                    table_name: "_uqa_mvcc_previous_versions",
                } => Authorization::Deny,
                _ => Authorization::Allow,
            }))?;
            assert!(schema::initialize(sqlite).is_err());
            sqlite.authorizer(None::<fn(AuthContext<'_>) -> Authorization>)?;
            assert_eq!(
                sqlite.query_row("SELECT format FROM _uqa_mvcc_metadata", [], |row| row
                    .get::<_, i64>(0))?,
                58
            );
            assert_eq!(
                schema::definition_matches(sqlite, VERSIONS_TABLE.0, PREVIOUS_VERSIONS_TABLE)?,
                Some(true)
            );
            assert_eq!(
                schema::definition_matches(sqlite, TABLE.0, PREVIOUS_TABLE)?,
                Some(true)
            );
            validate_triggers(sqlite, false)?;
            assert_eq!(
                sqlite.query_row(
                    "SELECT count(*) FROM sqlite_schema WHERE name = '_uqa_mvcc_previous_versions'",
                    [],
                    |row| row.get::<_, i64>(0)
                )?,
                0
            );
            Ok(())
        })
        .unwrap();
    let upgraded = SQLiteRecordStore::new(&connection).unwrap();
    assert_eq!(upgraded.database_id(), store.database_id());
    assert_eq!(receipt.sequence, CommitSequence::from_u64(1));
    assert_eq!(
        upgraded
            .commit_status(receipt.transaction, &control)
            .unwrap(),
        uqa_storage::mvcc::CommitStatus::Committed(receipt)
    );
    let snapshot = upgraded.snapshot(&control).unwrap();
    let record = snapshot.get(b"kept", &control).unwrap().unwrap();
    assert_eq!(&***record.value().unwrap(), b"original");
}
