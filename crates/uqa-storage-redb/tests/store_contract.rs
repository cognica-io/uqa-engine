//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_storage::{KeyValueStore, PersistentStorageProvider};
use uqa_storage_redb::RedbStorage;

#[test]
fn redb_passes_the_reusable_backend_contract() {
    let directory = tempfile::tempdir().unwrap();
    let storage = RedbStorage::open(directory.path().join("shared-contract.redb")).unwrap();
    let reader = storage.store();
    let writer = storage.store();
    uqa_storage::key_value::conformance::verify_store(&reader).unwrap();
    uqa_storage::key_value::conformance::verify_session_isolation(&reader, &writer).unwrap();
}

#[test]
fn ordered_scans_batches_and_reopen_preserve_bytes() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("contract.redb");
    {
        let storage = RedbStorage::open(&path).unwrap();
        let store = storage.store();
        store.put(b"fruit/2", b"green").unwrap();
        store.put(b"fruit/1", b"red").unwrap();
        store.put(b"other/1", b"yellow").unwrap();

        let mut batch = store.batch();
        batch.delete(b"fruit/2").unwrap();
        batch.put(b"fruit/3", &[0, 1, 0xff]).unwrap();
        batch.commit().unwrap();

        assert_eq!(
            store.scan_prefix(b"fruit/").unwrap(),
            vec![
                (b"fruit/1".to_vec(), b"red".to_vec()),
                (b"fruit/3".to_vec(), vec![0, 1, 0xff]),
            ]
        );
        assert_eq!(
            store
                .scan_prefix_keys_after(b"fruit/", Some(b"fruit/1"), 1)
                .unwrap(),
            vec![b"fruit/3".to_vec()]
        );
    }

    let storage = RedbStorage::open(&path).unwrap();
    assert_eq!(
        storage.store().get(b"fruit/3").unwrap(),
        Some(vec![0, 1, 0xff])
    );
}

#[test]
fn sessions_have_independent_mvcc_snapshots() {
    let directory = tempfile::tempdir().unwrap();
    let storage = RedbStorage::open(directory.path().join("sessions.redb")).unwrap();
    let reader = storage.store();
    let writer = storage.store();
    writer.put(b"key", b"before").unwrap();

    reader.begin_read_transaction().unwrap();
    assert_eq!(reader.get(b"key").unwrap().as_deref(), Some(&b"before"[..]));
    writer.put(b"key", b"after").unwrap();
    assert_eq!(reader.get(b"key").unwrap().as_deref(), Some(&b"before"[..]));
    reader.commit_transaction().unwrap();
    assert_eq!(reader.get(b"key").unwrap().as_deref(), Some(&b"after"[..]));
}

#[test]
fn savepoint_after_writes_uses_transaction_local_undo() {
    let directory = tempfile::tempdir().unwrap();
    let storage = RedbStorage::open(directory.path().join("savepoint.redb")).unwrap();
    let store = storage.store();

    store.begin_transaction().unwrap();
    store.put(b"key", b"before-savepoint").unwrap();
    store.put(b"prefix/0", b"survives").unwrap();
    store.savepoint("sp").unwrap();
    store.put(b"key", b"after-savepoint").unwrap();
    store.put(b"prefix/1", b"one").unwrap();
    store.put(b"prefix/2", b"two").unwrap();
    assert_eq!(store.delete_prefix(b"prefix/").unwrap(), 3);
    store.rollback_to_savepoint("sp").unwrap();
    assert_eq!(
        store.get(b"key").unwrap().as_deref(),
        Some(&b"before-savepoint"[..])
    );
    assert_eq!(
        store.scan_prefix(b"prefix/").unwrap(),
        vec![(b"prefix/0".to_vec(), b"survives".to_vec())]
    );

    store.put(b"key", b"second-attempt").unwrap();
    store.rollback_to_savepoint("sp").unwrap();
    assert_eq!(
        store.get(b"key").unwrap().as_deref(),
        Some(&b"before-savepoint"[..])
    );
    store.release_savepoint("sp").unwrap();
    store.commit_transaction().unwrap();
}

#[test]
fn commit_generation_ignores_reads_and_rollbacks() {
    let directory = tempfile::tempdir().unwrap();
    let storage = RedbStorage::open(directory.path().join("generation.redb")).unwrap();
    let store = storage.store();
    let initial = store.change_version().unwrap().unwrap();

    store.begin_read_transaction().unwrap();
    assert!(store.get(b"missing").unwrap().is_none());
    assert!(store.put(b"forbidden", b"value").is_err());
    store.commit_transaction().unwrap();
    assert_eq!(store.change_version().unwrap(), Some(initial));

    store.begin_transaction().unwrap();
    store.put(b"rolled-back", b"value").unwrap();
    assert!(store.transaction_has_written().unwrap());
    store.rollback_transaction().unwrap();
    assert_eq!(store.change_version().unwrap(), Some(initial));

    store.put(b"committed", b"value").unwrap();
    assert_eq!(store.change_version().unwrap(), Some(initial + 1));
}

#[test]
fn provider_returns_catalog_and_backend_bound_to_one_session() {
    let directory = tempfile::tempdir().unwrap();
    let storage = RedbStorage::open(directory.path().join("provider.redb")).unwrap();
    let session = storage.open_session().unwrap();

    session.backend.begin_transaction().unwrap();
    session
        .catalog
        .set_metadata("inside", "transaction")
        .unwrap();
    session.backend.rollback_transaction().unwrap();
    assert_eq!(
        storage
            .open_session()
            .unwrap()
            .catalog
            .get_metadata("inside")
            .unwrap(),
        None
    );
}
