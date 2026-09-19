//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

const LEGACY: TableDefinition<&[u8], &[u8]> = TableDefinition::new("uqa_key_value");
const OLD_META: TableDefinition<&str, u64> = TableDefinition::new("uqa_storage_metadata");

fn legacy(database: &Database) {
    let transaction = physical_writer(database).unwrap();
    {
        let mut table = transaction.open_table(LEGACY).unwrap();
        table.insert(b"a".as_slice(), b"old a".as_slice()).unwrap();
        table.insert(b"z".as_slice(), b"old z".as_slice()).unwrap();
        transaction
            .open_table(OLD_META)
            .unwrap()
            .insert("change_version", 7)
            .unwrap();
    }
    transaction.commit().unwrap();
}

#[test]
fn a_missing_corrupt_or_replaced_guard_is_rejected_without_repair() {
    const GUARD: TableDefinition<u8, u8> = TableDefinition::new("uqa_key_value");
    for damage in ["missing", "value", "legacy"] {
        let database = Arc::new(
            Database::builder()
                .create_with_backend(InMemoryBackend::new())
                .unwrap(),
        );
        let store = RedbRecordStore::new(Arc::clone(&database)).unwrap();
        store.migrate_key_value().unwrap();
        let transaction = physical_writer(&database).unwrap();
        transaction.delete_table(GUARD).unwrap();
        match damage {
            "value" => {
                transaction.open_table(GUARD).unwrap().insert(0, 2).unwrap();
            }
            "legacy" => {
                transaction.open_table(LEGACY).unwrap();
            }
            _ => {}
        }
        transaction.commit().unwrap();
        assert!(store.migrate_key_value().is_err());
        let transaction = database.begin_read().unwrap();
        match damage {
            "value" => assert_eq!(
                transaction
                    .open_table(GUARD)
                    .unwrap()
                    .get(0)
                    .unwrap()
                    .unwrap()
                    .value(),
                2
            ),
            "legacy" => {
                assert!(transaction.open_table(LEGACY).is_ok());
            }
            _ => {
                assert!(transaction.open_table(GUARD).is_err());
            }
        }
    }
}

#[test]
fn a_collision_after_copying_an_earlier_key_rolls_back_the_entire_migration() {
    let database = Arc::new(
        Database::builder()
            .create_with_backend(InMemoryBackend::new())
            .unwrap(),
    );
    legacy(&database);
    let store = RedbRecordStore::new(Arc::clone(&database)).unwrap();
    let control = StorageReadControl::with_limit(1 << 20);
    let id = store.allocate_transaction(&control).unwrap();
    store
        .commit(
            id,
            &PreparedRecordCommit::new(
                &[uqa_storage::mvcc::RecordWrite {
                    key: b"z",
                    expected: None,
                    value: Some(b"existing"),
                }],
                &control,
            )
            .unwrap(),
            &control,
        )
        .unwrap();
    assert!(store.migrate_key_value().is_err());
    let transaction = database.begin_read().unwrap();
    let table = transaction.open_table(LEGACY).unwrap();
    assert_eq!(
        table.get(b"a".as_slice()).unwrap().unwrap().value(),
        b"old a"
    );
    assert_eq!(
        table.get(b"z".as_slice()).unwrap().unwrap().value(),
        b"old z"
    );
    assert!(transaction
        .open_table(HEADS)
        .unwrap()
        .get(b"a".as_slice())
        .unwrap()
        .is_none());
    assert!(transaction
        .open_table(METADATA)
        .unwrap()
        .get("key_value_format")
        .unwrap()
        .is_none());
}

#[test]
fn a_failed_migration_commit_reopens_as_one_complete_format_and_can_resume() {
    let backend = FaultBackend::default();
    {
        let database = Arc::new(
            Database::builder()
                .create_with_backend(backend.clone())
                .unwrap(),
        );
        legacy(&database);
        let store = RedbRecordStore::new(database).unwrap();
        backend.fail_sync.store(true, Ordering::Relaxed);
        assert!(store.migrate_key_value().is_err());
        assert!(!backend.fail_sync.load(Ordering::Relaxed));
    }
    let database = Arc::new(Database::builder().create_with_backend(backend).unwrap());
    {
        let transaction = database.begin_read().unwrap();
        let migrated = transaction
            .open_table(METADATA)
            .unwrap()
            .get("key_value_format")
            .unwrap()
            .is_some();
        if migrated {
            assert!(transaction.open_table(LEGACY).is_err());
            assert!(transaction
                .open_table(HEADS)
                .unwrap()
                .get(b"a".as_slice())
                .unwrap()
                .is_some());
            assert!(transaction
                .open_table(HEADS)
                .unwrap()
                .get(b"z".as_slice())
                .unwrap()
                .is_some());
        } else {
            let table = transaction.open_table(LEGACY).unwrap();
            assert!(table.get(b"a".as_slice()).unwrap().is_some());
            assert!(table.get(b"z".as_slice()).unwrap().is_some());
            assert!(transaction
                .open_table(HEADS)
                .unwrap()
                .iter()
                .unwrap()
                .next()
                .is_none());
        }
    }
    let store = RedbRecordStore::new(database).unwrap();
    store.migrate_key_value().unwrap();
    store.migrate_key_value().unwrap();
    let control = StorageReadControl::with_limit(1 << 20);
    let snapshot = store.snapshot(&control).unwrap();
    for (key, expected) in [(b"a", b"old a"), (b"z", b"old z")] {
        assert_eq!(
            &***snapshot
                .get(key, &control)
                .unwrap()
                .unwrap()
                .value()
                .unwrap(),
            expected
        );
    }
}
