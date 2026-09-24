//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Format upgrades retain the original coordinator and reject corrupt singleton state.

use super::*;

const LEGACY: TableDefinition<u8, &[u8]> = TableDefinition::new("uqa_mvcc_serializable");
const RECORDS: TableDefinition<&[u8], &[u8]> =
    TableDefinition::new("uqa_mvcc_serializable_records");

fn legacy(
    store: &RedbRecordStore,
    control: &StorageReadControl,
) -> uqa_storage::mvcc::SerializableTransactionId {
    let (actor, bytes) = graph(store, control, |graph| {
        let actor = graph.admit(false, control)?;
        graph.observe_read(actor, SerializablePredicate::object([9; 16]), control)?;
        let mut bytes = Vec::new();
        graph.write_checkpoint(&mut bytes, control)?;
        Ok((actor, bytes))
    })
    .unwrap();
    let transaction = physical_writer(&store.database).unwrap();
    transaction.delete_table(RECORDS).unwrap();
    transaction
        .open_table(LEGACY)
        .unwrap()
        .insert(0, bytes.as_slice())
        .unwrap();
    let mut marker = [0; 24];
    marker[..8].copy_from_slice(b"UQARED01");
    marker[8..].copy_from_slice(&actor.coordinator());
    transaction
        .open_table(METADATA)
        .unwrap()
        .insert("serializable", marker.as_slice())
        .unwrap();
    transaction.commit().unwrap();
    actor
}

#[test]
fn restored_singleton_checkpoint_starts_a_new_coordinator_and_keeps_committed_data() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("legacy-backup.redb");
    let control = StorageReadControl::with_limit(1 << 20);
    let (request, original) = {
        let store = RedbRecordStore::new(Arc::new(Database::create(&path).unwrap())).unwrap();
        let transaction = store.allocate_transaction(&control).unwrap();
        let prepared = PreparedRecordCommit::new(
            &[RecordWrite {
                key: b"backup",
                expected: None,
                value: Some(b"durable"),
            }],
            &control,
        )
        .unwrap();
        store.commit(transaction, &prepared, &control).unwrap();
        let original = legacy(&store, &control);
        (
            uqa_storage::mvcc::DatabaseRestore::new(store.identity).unwrap(),
            original,
        )
    };
    let store =
        crate::mvcc::restore::open(Database::open(&path).unwrap(), request, &control).unwrap();
    let participant = actor(&store, &control);
    assert_ne!(participant.id().coordinator(), original.coordinator());
    assert_eq!(
        store
            .snapshot(&control)
            .unwrap()
            .get(b"backup", &control)
            .unwrap()
            .unwrap()
            .value()
            .map(|value| &***value),
        Some(b"durable".as_slice())
    );
    let transaction = store.database.begin_read().unwrap();
    assert!(matches!(
        transaction.open_table(LEGACY),
        Err(redb::TableError::TableDoesNotExist(_))
    ));
    assert!(transaction.open_table(RECORDS).unwrap().len().unwrap() > 1);
}

#[test]
fn checkpoint_upgrade_and_reopen_preserve_original_actor_identities() {
    for fail in [false, true] {
        let backend = FaultBackend::default();
        let control = StorageReadControl::with_limit(1 << 20);
        let store = RedbRecordStore::new(Arc::new(
            Database::builder()
                .create_with_backend(backend.clone())
                .unwrap(),
        ))
        .unwrap();
        let original = legacy(&store, &control);
        backend.fail_sync.store(fail, Ordering::Relaxed);
        let result = graph(&store, &control, |graph| graph.check_active(original));
        assert_eq!(result.is_err(), fail);
        drop(store);
        let store = RedbRecordStore::new(Arc::new(
            Database::builder().create_with_backend(backend).unwrap(),
        ))
        .unwrap();
        graph(&store, &control, |graph| graph.check_active(original)).unwrap();
        let transaction = store.database.begin_read().unwrap();
        assert!(matches!(
            transaction.open_table(LEGACY),
            Err(redb::TableError::TableDoesNotExist(_))
        ));
        assert!(transaction.open_table(RECORDS).unwrap().len().unwrap() > 1);
        drop(transaction);
        graph(&store, &control, |graph| {
            assert!(!graph.checkpoint_records_changed());
            let next = graph.admit(true, &control)?;
            assert_eq!(next.coordinator(), original.coordinator());
            assert!(next.allocation() > original.allocation());
            Ok(())
        })
        .unwrap();
    }
}

#[test]
fn invalid_singleton_state_is_rejected_before_migration_writes() {
    let backend = FaultBackend::default();
    let control = StorageReadControl::with_limit(1 << 20);
    let store = RedbRecordStore::new(Arc::new(
        Database::builder()
            .create_with_backend(backend.clone())
            .unwrap(),
    ))
    .unwrap();
    legacy(&store, &control);
    let transaction = physical_writer(&store.database).unwrap();
    transaction
        .open_table(LEGACY)
        .unwrap()
        .insert(0, b"corrupt".as_slice())
        .unwrap();
    transaction.commit().unwrap();
    let writes = backend.writes.load(Ordering::Relaxed);
    assert!(store
        .with_serializable_admission(&control, &mut |_, _| panic!("corrupt singleton admitted"))
        .is_err());
    assert_eq!(backend.writes.load(Ordering::Relaxed), writes);
    let transaction = store.database.begin_read().unwrap();
    assert!(transaction.open_table(LEGACY).is_ok());
    assert!(matches!(
        transaction.open_table(RECORDS),
        Err(redb::TableError::TableDoesNotExist(_))
    ));
}
