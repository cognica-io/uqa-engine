//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use rusqlite::params;
use uqa_storage::mvcc::{CommitFailure, CommitSequence, RecordWrite};

use super::*;

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
    let id = store.allocate_transaction(&control).unwrap();
    store
        .commit(id, &prepared(b"a", b"live", &control), &control)
        .unwrap();
    for sql in [
        "DELETE FROM _uqa_mvcc_versions",
        "UPDATE _uqa_mvcc_heads SET sequence = x'0000000000000009'",
        "DELETE FROM _uqa_mvcc_metadata",
        "DELETE FROM _uqa_mvcc_transactions",
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
        if sql.starts_with("DROP TABLE") {
            connection
                .with(|connection| {
                    assert_eq!(
                        connection.query_row(
                            "SELECT COUNT(*) FROM sqlite_schema WHERE name = '_uqa_mvcc_versions'",
                            [],
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
