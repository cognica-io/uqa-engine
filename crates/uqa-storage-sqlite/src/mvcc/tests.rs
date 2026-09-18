//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use rusqlite::params;
use uqa_storage::mvcc::{CommitFailure, CommitSequence, CommitStatus, RecordWrite};

use super::*;

mod identifiers;

fn control() -> StorageReadControl {
    StorageReadControl::with_limit(1 << 24)
}
fn prepared(key: &[u8], value: &[u8], control: &StorageReadControl) -> PreparedRecordCommit {
    PreparedRecordCommit::new(
        &[RecordWrite {
            key,
            expected: None,
            value: Some(value),
        }],
        control,
    )
    .unwrap()
}

#[test]
fn records_require_a_connection_local_permit_that_closes_after_every_operation() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("guard.db");
    let connection = ManagedConnection::open(&path).unwrap();
    let store = SQLiteRecordStore::new(&connection).unwrap();
    let control = control();
    store
        .allocate_identifiers(
            b"guard",
            uqa_storage::mvcc::IdentifierRequest::Observe(1),
            &control,
        )
        .unwrap();
    let id = store.allocate_transaction(&control).unwrap();
    store
        .commit(id, &prepared(b"a", b"live", &control), &control)
        .unwrap();
    for sql in [
        "DELETE FROM _uqa_mvcc_versions",
        "UPDATE _uqa_mvcc_heads SET sequence = x'0000000000000009'",
        "DELETE FROM _uqa_mvcc_metadata",
        "DELETE FROM _uqa_mvcc_transactions",
        "DELETE FROM _uqa_mvcc_identifiers",
    ] {
        assert!(connection
            .with(|connection| {
                connection.execute(sql, [])?;
                Ok(())
            })
            .is_err());
        assert!(rusqlite::Connection::open(&path)
            .unwrap()
            .execute(sql, [])
            .is_err());
    }
    connection
        .with(|connection| {
            assert_eq!(
                connection.query_row("PRAGMA synchronous", [], |row| row.get::<_, i64>(0))?,
                2
            );
            assert_eq!(
                connection.query_row("SELECT __uqa_mvcc_write_permit()", [], |row| row
                    .get::<_, i64>(0))?,
                0
            );
            Ok(())
        })
        .unwrap();
    assert_eq!(
        &***store
            .snapshot(&control)
            .unwrap()
            .get(b"a", &control)
            .unwrap()
            .unwrap()
            .value()
            .unwrap(),
        b"live"
    );
}

#[test]
fn incomplete_or_altered_formats_are_rejected_without_recreation() {
    for sql in [
        "DROP TABLE _uqa_mvcc_versions",
        "DROP TABLE _uqa_mvcc_identifiers",
        "DROP TRIGGER _uqa_mvcc_identifiers_UPDATE_guard",
        "DROP TRIGGER _uqa_mvcc_heads_INSERT_guard",
        "ALTER TABLE _uqa_mvcc_heads ADD COLUMN unrecognized INTEGER",
    ] {
        let connection = ManagedConnection::open_in_memory().unwrap();
        SQLiteRecordStore::new(&connection).unwrap();
        connection
            .with(|connection| {
                connection.execute_batch(sql)?;
                Ok(())
            })
            .unwrap();
        assert!(SQLiteRecordStore::new(&connection).is_err());
        if let Some(table) = sql.strip_prefix("DROP TABLE ") {
            connection
                .with(|connection| {
                    assert_eq!(
                        connection.query_row(
                            "SELECT COUNT(*) FROM sqlite_schema WHERE name = ?1",
                            [table],
                            |row| row.get::<_, i64>(0)
                        )?,
                        0
                    );
                    Ok(())
                })
                .unwrap();
        }
    }
}

#[test]
fn an_active_native_transaction_is_rejected_without_losing_its_session() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    connection.begin_transaction().unwrap();
    assert!(SQLiteRecordStore::new(&connection).is_err());
    assert!(connection.in_transaction());
    connection.rollback_transaction().unwrap();
    SQLiteRecordStore::new(&connection).unwrap();
}

fn downgrade_record_format(store: &SQLiteRecordStore, format: i64) {
    store.with(|connection| {
        let _permit = schema::WritePermit::acquire(connection)?;
        let definition: String = connection.query_row("SELECT sql FROM sqlite_schema WHERE name = '_uqa_mvcc_metadata'", [], |row| row.get(0))?;
        assert!(definition.contains("CHECK(format = 14)"));
        if format < 5 {
            assert_eq!(connection.query_row("SELECT count(*) FROM _uqa_mvcc_identifiers", [], |row| row.get::<_, i64>(0))?, 0);
            connection.execute_batch("DROP TABLE _uqa_mvcc_identifiers")?;
        }
        connection.execute_batch("ALTER TABLE _uqa_mvcc_metadata RENAME TO saved_metadata")?;
        connection.execute_batch(&definition.replace("CHECK(format = 14)", &format!("CHECK(format = {format})")))?;
        connection.execute_batch(&format!("INSERT INTO _uqa_mvcc_metadata SELECT singleton, {format}, database_id, allocated, sequence, mapping FROM saved_metadata; DROP TABLE saved_metadata;"))?;
        for action in ["INSERT", "UPDATE", "DELETE"] { connection.execute_batch(&schema::trigger("_uqa_mvcc_metadata", action).1)?; }
        Ok(())
    }).unwrap();
}

#[test]
fn allocated_predecessor_formats_reject_missing_identifier_state_or_guards() {
    for sql in [
        "DROP TABLE _uqa_mvcc_identifiers",
        "DROP TRIGGER _uqa_mvcc_identifiers_INSERT_guard",
    ] {
        let connection = ManagedConnection::open_in_memory().unwrap();
        let store = SQLiteRecordStore::new(&connection).unwrap();
        store
            .allocate_identifiers(
                b"reserved",
                uqa_storage::mvcc::IdentifierRequest::Observe(999),
                &control(),
            )
            .unwrap();
        downgrade_record_format(&store, 5);
        store
            .with(|connection| {
                let _permit = schema::WritePermit::acquire(connection)?;
                connection.execute_batch(sql)?;
                Ok(())
            })
            .unwrap();
        assert!(SQLiteRecordStore::new(&connection).is_err());
        store
            .with(|connection| {
                assert_eq!(
                    connection.query_row("SELECT format FROM _uqa_mvcc_metadata", [], |row| row
                        .get::<_, i64>(0))?,
                    5
                );
                let name = if sql.starts_with("DROP TABLE") {
                    "_uqa_mvcc_identifiers"
                } else {
                    "_uqa_mvcc_identifiers_INSERT_guard"
                };
                assert_eq!(
                    connection.query_row(
                        "SELECT count(*) FROM sqlite_schema WHERE name=?1",
                        [name],
                        |row| row.get::<_, i64>(0)
                    )?,
                    0
                );
                Ok(())
            })
            .unwrap();
    }
}

#[test]
fn record_format_upgrade_preserves_history_identity_allocations_and_receipts() {
    let control = control();
    for (mode, format) in (0..3)
        .flat_map(|mode| [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13].map(|format| (mode, format)))
    {
        let connection = ManagedConnection::open_in_memory().unwrap();
        if mode == 2 {
            crate::Catalog::open(connection.clone()).unwrap();
        }
        let open = || match mode {
            0 => SQLiteRecordStore::new(&connection),
            1 => SQLiteRecordStore::for_key_value(&connection, &control),
            _ => SQLiteRecordStore::for_native(&connection, &control),
        };
        let store = open().unwrap();
        let transaction = store.allocate_transaction(&control).unwrap();
        let prepared = if mode == 2 {
            PreparedRecordCommit::new(&[], &control).unwrap()
        } else {
            prepared(b"migration", b"preserved", &control)
        };
        let receipt = store.commit(transaction, &prepared, &control).unwrap();
        let pending = store.allocate_transaction(&control).unwrap();
        let snapshot = store.snapshot(&control).unwrap();
        let before = snapshot.scan(b"", None, 64, &control).unwrap();
        if format >= 5 {
            store
                .allocate_identifiers(
                    b"migration-identities",
                    uqa_storage::mvcc::IdentifierRequest::Observe(999),
                    &control,
                )
                .unwrap();
        }
        downgrade_record_format(&store, format);
        let upgraded = open().unwrap();
        if format >= 5 {
            assert_eq!(
                upgraded
                    .allocate_identifiers(
                        b"migration-identities",
                        uqa_storage::mvcc::IdentifierRequest::Observe(0),
                        &control
                    )
                    .unwrap()
                    .watermark(),
                999
            );
        }
        assert_eq!(upgraded.database_id(), store.database_id());
        assert_eq!(
            upgraded.snapshot(&control).unwrap().sequence(),
            snapshot.sequence()
        );
        assert_eq!(
            upgraded.commit_status(transaction, &control).unwrap(),
            CommitStatus::Committed(receipt)
        );
        assert_eq!(
            upgraded.commit_status(pending, &control).unwrap(),
            CommitStatus::Pending
        );
        let after = upgraded
            .snapshot(&control)
            .unwrap()
            .scan(b"", None, 64, &control)
            .unwrap();
        assert_eq!(before.len(), after.len());
        for (old, new) in before.iter().zip(after.iter()) {
            assert_eq!(&*old.key, &*new.key);
            assert_eq!(old.version.sequence(), new.version.sequence());
            assert_eq!(
                old.version.value().map(|value| &***value),
                new.version.value().map(|value| &***value)
            );
        }
        assert!(
            upgraded
                .allocate_transaction(&control)
                .unwrap()
                .allocation()
                > pending.allocation()
        );
        upgraded
            .with(|connection| {
                assert_eq!(
                    connection.query_row("SELECT format FROM _uqa_mvcc_metadata", [], |row| row
                        .get::<_, i64>(0))?,
                    14
                );
                Ok(())
            })
            .unwrap();
        open().unwrap();
    }
}

#[test]
fn failed_record_format_upgrade_restores_the_old_schema_and_allows_repair() {
    for format in [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13] {
        let connection = ManagedConnection::open_in_memory().unwrap();
        let store = SQLiteRecordStore::new(&connection).unwrap();
        downgrade_record_format(&store, format);
        store.with(|connection| {
        let _permit = schema::WritePermit::acquire(connection)?;
        connection.execute_batch("PRAGMA ignore_check_constraints = ON; UPDATE _uqa_mvcc_metadata SET mapping = 99; PRAGMA ignore_check_constraints = OFF;")?;
        Ok(())
    }).unwrap();
        assert!(SQLiteRecordStore::new(&connection).is_err());
        store
            .with(|connection| {
                assert_eq!(
                    connection.query_row("SELECT format FROM _uqa_mvcc_metadata", [], |row| row
                        .get::<_, i64>(0))?,
                    format
                );
                assert_eq!(
                connection.query_row(
                    "SELECT count(*) FROM sqlite_schema WHERE name IN ('_uqa_mvcc_previous_metadata', '_uqa_mvcc_identifiers')",
                    [],
                    |row| row.get::<_, i64>(0)
                )?,
                i64::from(format >= 5)
            );
                let _permit = schema::WritePermit::acquire(connection)?;
                connection.execute_batch("UPDATE _uqa_mvcc_metadata SET mapping = 0")?;
                Ok(())
            })
            .unwrap();
        SQLiteRecordStore::new(&connection).unwrap();
    }
}

#[test]
fn closed_record_format_files_upgrade_in_every_sqlite_mode() {
    let control = control();
    for (mode, format) in (0..4)
        .flat_map(|mode| [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13].map(|format| (mode, format)))
    {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("record-upgrade.db");
        let open = || {
            match mode {
                0 => ManagedConnection::open(&path),
                1 => ManagedConnection::open_encrypted(&path, "record upgrade key"),
                2 => ManagedConnection::open_compressed(
                    &path,
                    crate::SQLiteCompressionOptions::default(),
                ),
                _ => ManagedConnection::open_compressed_encrypted(
                    &path,
                    "record upgrade key",
                    crate::SQLiteCompressionOptions::default(),
                ),
            }
            .unwrap()
        };
        let (identity, receipt) = {
            let connection = open();
            let store = SQLiteRecordStore::for_key_value(&connection, &control).unwrap();
            let transaction = store.allocate_transaction(&control).unwrap();
            let receipt = store
                .commit(
                    transaction,
                    &prepared(b"retained", b"original bytes", &control),
                    &control,
                )
                .unwrap();
            downgrade_record_format(&store, format);
            (store.database_id(), receipt)
        };
        for _ in 0..2 {
            let connection = open();
            let store = SQLiteRecordStore::for_key_value(&connection, &control).unwrap();
            assert_eq!(store.database_id(), identity);
            assert_eq!(
                store.commit_status(receipt.transaction, &control).unwrap(),
                CommitStatus::Committed(receipt)
            );
            let value = store
                .snapshot(&control)
                .unwrap()
                .get(b"retained", &control)
                .unwrap()
                .unwrap();
            assert_eq!(&***value.value().unwrap(), b"original bytes");
            assert_eq!(value.sequence(), receipt.sequence);
        }
    }
}

#[test]
fn unwinding_rolls_back_physical_changes_and_closes_write_permission() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let store = SQLiteRecordStore::new(&connection).unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _: VersionResult<()> = store.with(|connection| {
            let _permit = schema::WritePermit::acquire(connection)?;
            let transaction = schema::begin(connection)?;
            transaction.execute(
                "UPDATE _uqa_mvcc_metadata SET sequence = ?1",
                params![7_u64.to_be_bytes().as_slice()],
            )?;
            panic!("injected provider unwind");
        });
    }));
    assert!(result.is_err());
    assert_eq!(
        store.snapshot(&control()).unwrap().sequence(),
        CommitSequence::INITIAL
    );
    connection
        .with(|connection| {
            assert_eq!(
                connection.query_row("SELECT __uqa_mvcc_write_permit()", [], |row| row
                    .get::<_, i64>(0))?,
                0
            );
            assert!(connection
                .execute("DELETE FROM _uqa_mvcc_metadata", [])
                .is_err());
            Ok(())
        })
        .unwrap();
}

#[test]
fn unsigned_allocations_and_sequences_reach_their_limit_without_wrapping() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let store = SQLiteRecordStore::new(&connection).unwrap();
    let control = control();
    let first = store.allocate_transaction(&control).unwrap();
    store
        .with(|connection| {
            let _permit = schema::WritePermit::acquire(connection)?;
            connection.execute(
                "UPDATE _uqa_mvcc_metadata SET allocated = ?1, sequence = ?1",
                params![(u64::MAX - 1).to_be_bytes().as_slice()],
            )?;
            Ok(())
        })
        .unwrap();
    let last = store.allocate_transaction(&control).unwrap();
    assert_eq!(last.allocation(), u64::MAX);
    let receipt = store
        .commit(first, &prepared(b"a", b"last", &control), &control)
        .unwrap();
    assert_eq!(receipt.sequence, CommitSequence::from_u64(u64::MAX));
    assert!(matches!(
        store.allocate_transaction(&control),
        Err(VersionError::TransactionIdsExhausted)
    ));
    assert!(matches!(
        store.commit(last, &prepared(b"b", b"never", &control), &control),
        Err(CommitFailure::Rejected(VersionError::SequenceExhausted))
    ));
    assert!(store
        .snapshot(&control)
        .unwrap()
        .get(b"b", &control)
        .unwrap()
        .is_none());
    assert_eq!(store.abort(last, &control).unwrap(), CommitStatus::Aborted);
}

#[test]
fn sqlite_full_rolls_back_partial_staging_and_preserves_the_pending_receipt() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let store = SQLiteRecordStore::new(&connection).unwrap();
    let control = control();
    let id = store.allocate_transaction(&control).unwrap();
    connection
        .with(|connection| {
            let pages: i64 = connection.query_row("PRAGMA page_count", [], |row| row.get(0))?;
            connection.pragma_update(None, "max_page_count", pages)?;
            Ok(())
        })
        .unwrap();
    let large = vec![8; 1 << 20];
    let prepared = PreparedRecordCommit::new(
        &[
            RecordWrite {
                key: b"a",
                expected: None,
                value: Some(b"small"),
            },
            RecordWrite {
                key: b"z",
                expected: None,
                value: Some(&large),
            },
        ],
        &control,
    )
    .unwrap();
    assert!(matches!(
        store.commit(id, &prepared, &control),
        Err(CommitFailure::Rejected(VersionError::Storage(_)))
    ));
    assert_eq!(
        store.commit_status(id, &control).unwrap(),
        CommitStatus::Pending
    );
    assert!(store
        .snapshot(&control)
        .unwrap()
        .get(b"a", &control)
        .unwrap()
        .is_none());
    connection
        .with(|connection| {
            connection.pragma_update(None, "max_page_count", 10000)?;
            Ok(())
        })
        .unwrap();
    store.commit(id, &prepared, &control).unwrap();
    assert_eq!(
        &***store
            .snapshot(&control)
            .unwrap()
            .get(b"z", &control)
            .unwrap()
            .unwrap()
            .value()
            .unwrap(),
        large.as_slice()
    );
}
