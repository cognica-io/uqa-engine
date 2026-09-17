//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Recovery preserves either complete identifier reservation state after a physical synchronization failure.

use super::*;
use uqa_storage::mvcc::IdentifierRequest;

fn reserve(count: u64) -> IdentifierRequest {
    IdentifierRequest::Reserve {
        minimum: 1,
        maximum: u64::MAX,
        count: std::num::NonZeroU64::new(count).unwrap(),
    }
}

#[test]
fn failed_identifier_sync_recovers_one_complete_watermark_without_reusing_returned_ids() {
    let backend = FaultBackend::default();
    let control = StorageReadControl::with_limit(1 << 20);
    {
        let database = Arc::new(
            Database::builder()
                .create_with_backend(backend.clone())
                .unwrap(),
        );
        let store = RedbRecordStore::new(database).unwrap();
        assert_eq!(
            store
                .allocate_identifiers(b"entities", reserve(2), &control)
                .unwrap()
                .range(),
            Some(1..=2)
        );
        backend.fail_sync.store(true, Ordering::Relaxed);
        assert!(store
            .allocate_identifiers(b"entities", reserve(3), &control)
            .is_err());
    }
    let database = Arc::new(Database::builder().create_with_backend(backend).unwrap());
    let store = RedbRecordStore::new(database.clone()).unwrap();
    let previous = {
        let read = database.begin_read().unwrap();
        let table = read.open_table(super::super::identifiers::TABLE).unwrap();
        codec::decode_u64(table.get(b"entities".as_slice()).unwrap().unwrap().value()).unwrap()
    };
    assert!(
        previous == 2 || previous == 5,
        "partial reservation survived: {previous}"
    );
    assert_eq!(
        store
            .allocate_identifiers(b"entities", reserve(1), &control)
            .unwrap()
            .range(),
        Some(previous + 1..=previous + 1)
    );
}

#[test]
fn a_missing_identifier_table_is_not_recreated_for_a_current_format() {
    let database = Arc::new(
        Database::builder()
            .create_with_backend(InMemoryBackend::new())
            .unwrap(),
    );
    let retained = RedbRecordStore::new(database.clone()).unwrap();
    let transaction = database.begin_write().unwrap();
    transaction
        .delete_table(super::super::identifiers::TABLE)
        .unwrap();
    transaction.commit().unwrap();
    assert!(matches!(
        retained.allocate_identifiers(
            b"entities",
            reserve(1),
            &StorageReadControl::with_limit(1 << 20)
        ),
        Err(VersionError::InvalidEncoding(_))
    ));
    assert!(matches!(
        RedbRecordStore::new(database.clone()),
        Err(VersionError::InvalidEncoding(_))
    ));
    assert!(database
        .begin_read()
        .unwrap()
        .open_table(super::super::identifiers::TABLE)
        .is_err());
}
