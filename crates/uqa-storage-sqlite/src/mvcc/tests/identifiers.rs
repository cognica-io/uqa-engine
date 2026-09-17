//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Failed allocation publication and stale database identities never expose a partially advanced watermark.

use super::*;
use uqa_storage::mvcc::{DatabaseId, IdentifierRequest};

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
    store.identity = identity;
    assert_eq!(
        store
            .allocate_identifiers(b"entities", reserve(1), &control())
            .unwrap()
            .range(),
        Some(1..=1)
    );
}
