//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Failed allocation publication and stale database identities never expose a partially advanced watermark.

use super::*;
use uqa_storage::mvcc::{DatabaseId, IdentifierRequest};

mod diskann;

fn synchronous(connection: &rusqlite::Connection) -> i64 {
    connection
        .pragma_query_value(None, "synchronous", |row| row.get(0))
        .unwrap()
}

#[test]
fn wal_observations_commit_without_their_own_sync_and_restore_full_synchronization() {
    use crate::mvcc::{identifiers::ObservationSynchronization, schema::WritePermit};
    let directory = tempfile::tempdir().unwrap();
    let connection = ManagedConnection::open(&directory.path().join("observations.db")).unwrap();
    let store = SQLiteRecordStore::new(&connection).unwrap();
    let control = control();
    store
        .with(|connection| {
            let permit = WritePermit::acquire(connection)?;
            assert_eq!(synchronous(connection), 2);
            let relaxed = ObservationSynchronization::relax(connection, &permit)?;
            assert!(relaxed.is_some(), "WAL observations relax synchronization");
            assert_eq!(synchronous(connection), 1);
            drop(relaxed);
            assert_eq!(synchronous(connection), 2);
            // A restoration attempted inside a transaction fails and stays recorded until the next write admission restores it.
            let relaxed = ObservationSynchronization::relax(connection, &permit)?;
            connection.execute_batch("BEGIN")?;
            drop(relaxed);
            connection.execute_batch("COMMIT")?;
            assert_eq!(synchronous(connection), 1);
            drop(permit);
            drop(WritePermit::acquire(connection)?);
            assert_eq!(synchronous(connection), 2);
            Ok(())
        })
        .unwrap();
    for (request, watermark) in [(IdentifierRequest::Observe(40), 40), (reserve(2), 42)] {
        assert_eq!(
            store
                .allocate_identifiers(b"entities", request, &control)
                .unwrap()
                .watermark(),
            watermark
        );
        store
            .with(|connection| {
                assert_eq!(synchronous(connection), 2);
                Ok(())
            })
            .unwrap();
    }
    drop(store);
    drop(connection);
    let reopened = ManagedConnection::open(&directory.path().join("observations.db")).unwrap();
    assert_eq!(
        SQLiteRecordStore::new(&reopened)
            .unwrap()
            .identifier_watermark(b"entities", &control)
            .unwrap(),
        Some(42)
    );
}

#[test]
fn rollback_journal_observations_keep_full_synchronization() {
    use crate::mvcc::{identifiers::ObservationSynchronization, schema::WritePermit};
    let directory = tempfile::tempdir().unwrap();
    let connection = ManagedConnection::open_compressed(
        &directory.path().join("observations.db"),
        crate::SQLiteCompressionOptions::default(),
    )
    .unwrap();
    let store = SQLiteRecordStore::new(&connection).unwrap();
    store
        .with(|connection| {
            let permit = WritePermit::acquire(connection)?;
            assert!(ObservationSynchronization::relax(connection, &permit)?.is_none());
            assert_eq!(synchronous(connection), 2);
            Ok(())
        })
        .unwrap();
    assert_eq!(
        store
            .allocate_identifiers(b"entities", IdentifierRequest::Observe(7), &control())
            .unwrap()
            .watermark(),
        7
    );
}

fn reserve(count: u64) -> IdentifierRequest {
    IdentifierRequest::Reserve {
        minimum: 1,
        maximum: u64::MAX,
        count: std::num::NonZeroU64::new(count).unwrap(),
    }
}

#[test]
fn failed_identifier_publication_preserves_the_watermark_and_releases_permission() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let store = SQLiteRecordStore::new(&connection).unwrap();
    let control = control();
    assert_eq!(
        store
            .allocate_identifiers(b"entities", reserve(2), &control)
            .unwrap()
            .range(),
        Some(1..=2)
    );
    store.with(|connection| {
        connection.execute_batch("CREATE TRIGGER reject_identifier_publication AFTER UPDATE ON _uqa_mvcc_identifiers BEGIN SELECT RAISE(ABORT, 'injected allocation failure'); END")?;
        Ok(())
    }).unwrap();
    assert!(store
        .allocate_identifiers(b"entities", reserve(3), &control)
        .is_err());
    store
        .with(|connection| {
            assert_eq!(
                connection.query_row("SELECT __uqa_mvcc_write_permit()", [], |row| row
                    .get::<_, i64>(0))?,
                0
            );
            assert_eq!(
                connection.query_row(
                    "SELECT watermark FROM _uqa_mvcc_identifiers WHERE namespace = ?1",
                    [b"entities".as_slice()],
                    |row| row.get::<_, Vec<u8>>(0)
                )?,
                2_u64.to_be_bytes()
            );
            connection.execute_batch("DROP TRIGGER reject_identifier_publication")?;
            Ok(())
        })
        .unwrap();
    assert_eq!(
        store
            .allocate_identifiers(b"entities", reserve(1), &control)
            .unwrap()
            .range(),
        Some(3..=3)
    );
}

#[test]
fn stale_database_identity_cannot_consume_identifiers() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let mut store = SQLiteRecordStore::new(&connection).unwrap();
    let identity = store.identity;
    let mut wrong = identity.as_bytes();
    wrong[0] ^= 1;
    store.identity = DatabaseId::from_bytes(wrong);
    assert!(matches!(
        store.allocate_identifiers(b"entities", reserve(1), &control()),
        Err(VersionError::WrongDatabase)
    ));
    assert!(matches!(
        store.identifier_watermark(b"entities", &control()),
        Err(VersionError::WrongDatabase)
    ));
    store.identity = identity;
    assert_eq!(
        store
            .allocate_identifiers(b"entities", reserve(1), &control())
            .unwrap()
            .range(),
        Some(1..=1)
    );
}

#[test]
fn watermark_reads_work_on_query_only_connections_without_acquiring_write_permission() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let store = SQLiteRecordStore::new(&connection).unwrap();
    let control = control();
    store
        .allocate_identifiers(b"entities", reserve(2), &control)
        .unwrap();
    store
        .with(|connection| {
            connection.pragma_update(None, "query_only", true)?;
            Ok(())
        })
        .unwrap();
    assert_eq!(
        store.identifier_watermark(b"entities", &control).unwrap(),
        Some(2)
    );
    assert_eq!(
        store
            .identifier_watermark(b"unallocated", &control)
            .unwrap(),
        None
    );
    store
        .with(|connection| {
            assert_eq!(
                connection.query_row("SELECT __uqa_mvcc_write_permit()", [], |row| row
                    .get::<_, i64>(0))?,
                0
            );
            assert_eq!(
                connection.query_row("SELECT count(*) FROM _uqa_mvcc_identifiers", [], |row| {
                    row.get::<_, i64>(0)
                })?,
                1
            );
            assert!(connection.is_autocommit());
            Ok(())
        })
        .unwrap();
}
