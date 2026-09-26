//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::downgrade_record_format;
use super::*;
use uqa_storage::key_value::diskann_identifiers::{LEGACY_PREFIX, NAMESPACE};

fn legacy(index: u64) -> Vec<u8> {
    let mut key = LEGACY_PREFIX.to_vec();
    key.extend_from_slice(&[9; 16]);
    key.extend_from_slice(&11_u64.to_be_bytes());
    key.extend_from_slice(&index.to_be_bytes());
    key
}

fn populate(store: &SQLiteRecordStore, control: &StorageReadControl) {
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
        let connection = ManagedConnection::open_in_memory().unwrap();
        let store = SQLiteRecordStore::new(&connection).unwrap();
        let control = control();
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
        let id = store.allocate_transaction(&control).unwrap();
        let writes = prepared(b"kept", b"original", &control);
        store.commit(id, &writes, &control).unwrap();
        let receipt = store.commit_status(id, &control).unwrap();
        downgrade_record_format(&store, 50);
        let upgraded = SQLiteRecordStore::new(&connection).unwrap();
        assert_eq!(upgraded.database_id(), store.database_id());
        assert_eq!(upgraded.commit_status(id, &control).unwrap(), receipt);
        assert_eq!(
            upgraded
                .snapshot(&control)
                .unwrap()
                .get(b"kept", &control)
                .unwrap()
                .unwrap()
                .value()
                .map(|value| &***value),
            Some(b"original".as_slice())
        );
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
fn diskann_identifier_upgrade_failure_restores_the_format_and_original_watermarks() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let store = SQLiteRecordStore::new(&connection).unwrap();
    let control = control();
    populate(&store, &control);
    store
        .allocate_identifiers(NAMESPACE, IdentifierRequest::Observe(3), &control)
        .unwrap();
    downgrade_record_format(&store, 50);
    store.with(|connection| {
        connection.execute_batch("CREATE TRIGGER reject_diskann_identifier_upgrade AFTER DELETE ON _uqa_mvcc_identifiers WHEN OLD.watermark = x'00000000000000e6' BEGIN SELECT RAISE(ABORT, 'injected identifier consolidation failure'); END")?;
        Ok(())
    }).unwrap();
    assert!(SQLiteRecordStore::new(&connection).is_err());
    store
        .with(|connection| {
            assert_eq!(
                connection.query_row("SELECT format FROM _uqa_mvcc_metadata", [], |row| row
                    .get::<_, i64>(0))?,
                50
            );
            for (key, expected) in (1..=130)
                .map(|index| (legacy(index), index + 100))
                .chain([(NAMESPACE.to_vec(), 3)])
            {
                let value: Vec<u8> = connection.query_row(
                    "SELECT watermark FROM _uqa_mvcc_identifiers WHERE namespace=?1",
                    [key],
                    |row| row.get(0),
                )?;
                assert_eq!(value, expected.to_be_bytes());
            }
            assert_eq!(
                connection.query_row("SELECT __uqa_mvcc_write_permit()", [], |row| row
                    .get::<_, i64>(0))?,
                0
            );
            connection.execute_batch("DROP TRIGGER reject_diskann_identifier_upgrade")?;
            Ok(())
        })
        .unwrap();
    let upgraded = SQLiteRecordStore::new(&connection).unwrap();
    assert_eq!(
        upgraded.identifier_watermark(&legacy(1), &control).unwrap(),
        None
    );
    assert_eq!(
        upgraded.identifier_watermark(NAMESPACE, &control).unwrap(),
        Some(230)
    );
}
