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
fn a_missing_identifier_table_is_not_recreated_when_the_format_requires_allocations() {
    for format in [5_u64, 6, 7] {
        let database = Arc::new(
            Database::builder()
                .create_with_backend(InMemoryBackend::new())
                .unwrap(),
        );
        let retained = RedbRecordStore::new(database.clone()).unwrap();
        let transaction = database.begin_write().unwrap();
        transaction
            .open_table(METADATA)
            .unwrap()
            .insert("format", format.to_be_bytes().as_slice())
            .unwrap();
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
        assert!(retained
            .identifier_watermark(b"entities", &StorageReadControl::with_limit(1 << 20))
            .is_err());
        assert!(database
            .begin_read()
            .unwrap()
            .open_table(super::super::identifiers::TABLE)
            .is_err());
    }
}

#[test]
fn watermark_reads_do_not_synchronize_or_publish_identifier_state() {
    let backend = FaultBackend::default();
    let database = Arc::new(
        Database::builder()
            .create_with_backend(backend.clone())
            .unwrap(),
    );
    let store = RedbRecordStore::new(database).unwrap();
    let control = StorageReadControl::with_limit(1 << 20);
    store
        .allocate_identifiers(b"entities", reserve(2), &control)
        .unwrap();
    backend.fail_sync.store(true, Ordering::Relaxed);
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
    assert!(backend.fail_sync.load(Ordering::Relaxed));
    backend.fail_sync.store(false, Ordering::Relaxed);
    assert_eq!(
        store
            .allocate_identifiers(b"unallocated", reserve(1), &control)
            .unwrap()
            .watermark(),
        1
    );
    let mut wrong = store.clone();
    let mut identity = store.identity.as_bytes();
    identity[0] ^= 1;
    wrong.identity = uqa_storage::mvcc::DatabaseId::from_bytes(identity);
    assert!(matches!(
        wrong.identifier_watermark(b"entities", &control),
        Err(VersionError::WrongDatabase)
    ));
}
