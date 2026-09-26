//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_storage::key_value::diskann_identifiers::{self, LEGACY_PREFIX, NAMESPACE};

fn legacy(index: u64) -> Vec<u8> {
    let mut key = LEGACY_PREFIX.to_vec();
    key.extend_from_slice(&[9; 16]);
    key.extend_from_slice(&11_u64.to_be_bytes());
    key.extend_from_slice(&index.to_be_bytes());
    key
}

fn downgrade(database: &Database) {
    let transaction = database.begin_write().unwrap();
    transaction
        .open_table(METADATA)
        .unwrap()
        .insert("format", 49_u64.to_be_bytes().as_slice())
        .unwrap();
    transaction.commit().unwrap();
}

fn populate(store: &RedbRecordStore, control: &StorageReadControl) {
    for index in 1..=130 {
        store
            .allocate_identifiers(
                &legacy(index),
                IdentifierRequest::Observe(index + 100),
                control,
            )
            .unwrap();
    }
}

#[test]
fn diskann_identifier_upgrade_folds_all_pages_without_rewinding_or_touching_other_domains() {
    for existing in [None, Some(1000), Some(u64::MAX)] {
        let database = Arc::new(
            Database::builder()
                .create_with_backend(InMemoryBackend::new())
                .unwrap(),
        );
        let store = RedbRecordStore::new(database.clone()).unwrap();
        let control = StorageReadControl::with_limit(1 << 20);
        if let Some(existing) = existing {
            store
                .allocate_identifiers(NAMESPACE, IdentifierRequest::Observe(existing), &control)
                .unwrap();
        }
        populate(&store, &control);
        let mut longer = legacy(1);
        longer.push(0);
        for unrelated in [
            b"ordinary".as_slice(),
            LEGACY_PREFIX.as_slice(),
            longer.as_slice(),
        ] {
            store
                .allocate_identifiers(unrelated, IdentifierRequest::Observe(9999), &control)
                .unwrap();
        }
        downgrade(&database);
        let upgraded = RedbRecordStore::new(database.clone()).unwrap();
        assert_eq!(upgraded.database_id(), store.database_id());
        let maximum = existing.unwrap_or(0).max(230);
        assert_eq!(
            upgraded.identifier_watermark(NAMESPACE, &control).unwrap(),
            Some(maximum)
        );
        for index in 1..=130 {
            assert_eq!(
                upgraded
                    .identifier_watermark(&legacy(index), &control)
                    .unwrap(),
                None
            );
        }
        for unrelated in [
            b"ordinary".as_slice(),
            LEGACY_PREFIX.as_slice(),
            longer.as_slice(),
        ] {
            assert_eq!(
                upgraded.identifier_watermark(unrelated, &control).unwrap(),
                Some(9999)
            );
        }
        if maximum == u64::MAX {
            assert!(matches!(
                upgraded.allocate_identifiers(NAMESPACE, reserve(1), &control),
                Err(VersionError::IdentifiersExhausted)
            ));
            assert_eq!(
                upgraded.identifier_watermark(NAMESPACE, &control).unwrap(),
                Some(maximum)
            );
        } else {
            assert_eq!(
                upgraded
                    .allocate_identifiers(NAMESPACE, reserve(1), &control)
                    .unwrap()
                    .watermark(),
                maximum + 1
            );
        }
    }
}

#[test]
fn diskann_identifier_upgrade_sync_failure_recovers_one_complete_allocation_domain() {
    let backend = FaultBackend::default();
    let control = StorageReadControl::with_limit(1 << 20);
    {
        let database = Arc::new(
            Database::builder()
                .create_with_backend(backend.clone())
                .unwrap(),
        );
        let store = RedbRecordStore::new(database.clone()).unwrap();
        populate(&store, &control);
        store
            .allocate_identifiers(NAMESPACE, IdentifierRequest::Observe(3), &control)
            .unwrap();
        downgrade(&database);
        backend.fail_sync.store(true, Ordering::Relaxed);
        assert!(RedbRecordStore::new(database.clone()).is_err());
    }
    let database = Arc::new(Database::builder().create_with_backend(backend).unwrap());
    {
        let read = database.begin_read().unwrap();
        let format = read_u64(&read.open_table(METADATA).unwrap(), "format").unwrap();
        let table = read.open_table(crate::mvcc::identifiers::TABLE).unwrap();
        let legacy_count = table
            .iter()
            .unwrap()
            .map(|entry| entry.unwrap())
            .filter(|(key, _)| diskann_identifiers::is_legacy_namespace(key.value()))
            .count();
        let watermark = codec::decode_u64(table.get(NAMESPACE).unwrap().unwrap().value()).unwrap();
        match format {
            49 => assert_eq!((legacy_count, watermark), (130, 3)),
            51 => assert_eq!((legacy_count, watermark), (0, 230)),
            _ => panic!("incomplete generation identifier upgrade: {format}"),
        }
    }
    let recovered = RedbRecordStore::new(database).unwrap();
    assert_eq!(
        recovered
            .allocate_identifiers(NAMESPACE, reserve(1), &control)
            .unwrap()
            .watermark(),
        231
    );
}
