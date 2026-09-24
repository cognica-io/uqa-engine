//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Public restore construction rejects live owners and keeps completed retries distinct from new restorations.

use uqa_storage::{
    mvcc::{DatabaseId, DatabaseRestore, SerializableCoordinator, VersionedPersistence},
    read_control::StorageReadControl,
    KeyValueStore,
};
use uqa_storage_redb::{RedbStorage, VersionedSessionOptions};

#[test]
fn closed_backup_restore_preserves_provider_data_and_idempotent_retries() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("source.redb");
    let backup = directory.path().join("backup.redb");
    let control = StorageReadControl::with_limit(1 << 20);
    let request = {
        let storage = RedbStorage::open(&path).unwrap();
        let session = storage.store();
        session.put(b"first", b"backup").unwrap();
        session.put(b"deleted", b"old").unwrap();
        session.delete(b"deleted").unwrap();
        DatabaseRestore::new(storage.record_store().unwrap().database_id()).unwrap()
    };
    std::fs::copy(&path, &backup).unwrap();
    let late_transaction = {
        let storage = RedbStorage::open_restored(
            &backup,
            request,
            VersionedSessionOptions::default(),
            &control,
        )
        .unwrap();
        let session = storage.store();
        assert_eq!(
            session.get(b"first").unwrap().as_deref(),
            Some(b"backup".as_slice())
        );
        assert!(session.get(b"deleted").unwrap().is_none());
        session.put(b"later", b"keep on retry").unwrap();
        let records = storage.record_store().unwrap();
        assert_eq!(records.database_id(), request.target());
        records.allocate_transaction(&control).unwrap()
    };
    for retry in [false, true] {
        let storage = if retry {
            RedbStorage::open_restored(
                &backup,
                request,
                VersionedSessionOptions::default(),
                &control,
            )
            .unwrap()
        } else {
            RedbStorage::open(&backup).unwrap()
        };
        assert_eq!(
            storage.store().get(b"later").unwrap().as_deref(),
            Some(b"keep on retry".as_slice())
        );
        assert_eq!(
            storage
                .record_store()
                .unwrap()
                .commit_status(late_transaction, &control)
                .unwrap(),
            uqa_storage::mvcc::CommitStatus::Pending
        );
    }
    let source = RedbStorage::open(&path).unwrap();
    assert_eq!(
        source.record_store().unwrap().database_id(),
        request.source()
    );
    assert!(source.store().get(b"later").unwrap().is_none());
}

#[test]
fn every_retained_owner_prevents_restore_before_mutation() {
    for holder in ["provider", "session", "snapshot", "participant"] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("held.redb");
        let control = StorageReadControl::with_limit(1 << 20);
        let storage = RedbStorage::open(&path).unwrap();
        storage.store().put(b"preserved", b"original").unwrap();
        let records = storage.record_store().unwrap();
        let request = DatabaseRestore::new(records.database_id()).unwrap();
        let retained: Box<dyn std::any::Any> = match holder {
            "provider" => Box::new(storage.clone()),
            "session" => Box::new(storage.store()),
            "snapshot" => Box::new(records.snapshot(&control).unwrap()),
            _ => Box::new(
                records
                    .admit_serializable_snapshot(false, &control)
                    .unwrap()
                    .0,
            ),
        };
        drop((records, storage));
        assert!(
            RedbStorage::open_restored(
                &path,
                request,
                VersionedSessionOptions::default(),
                &control
            )
            .is_err(),
            "live {holder} did not exclude restore"
        );
        drop(retained);
        {
            let unchanged = RedbStorage::open(&path).unwrap();
            assert_eq!(
                unchanged.record_store().unwrap().database_id(),
                request.source()
            );
            assert_eq!(
                unchanged.store().get(b"preserved").unwrap().as_deref(),
                Some(b"original".as_slice())
            );
        }
        let restored = RedbStorage::open_restored(
            &path,
            request,
            VersionedSessionOptions::default(),
            &control,
        )
        .unwrap();
        assert_eq!(
            restored.record_store().unwrap().database_id(),
            request.target()
        );
    }
}

#[test]
fn restore_rejects_unrelated_missing_and_uninitialized_files_without_creating_a_database() {
    let directory = tempfile::tempdir().unwrap();
    let missing = directory.path().join("missing.redb");
    let empty = directory.path().join("empty.redb");
    let unrelated = directory.path().join("unrelated.redb");
    let control = StorageReadControl::with_limit(1 << 20);
    let request = DatabaseRestore::from_identities(
        DatabaseId::from_bytes([1; 16]),
        DatabaseId::from_bytes([2; 16]),
    )
    .unwrap();
    assert!(RedbStorage::open_restored(
        &missing,
        request,
        VersionedSessionOptions::default(),
        &control
    )
    .is_err());
    assert!(!missing.exists());
    drop(redb::Database::create(&empty).unwrap());
    assert!(RedbStorage::open_restored(
        &empty,
        request,
        VersionedSessionOptions::default(),
        &control
    )
    .is_err());
    {
        use redb::ReadableDatabase;
        let database = redb::Database::open(&empty).unwrap();
        assert_eq!(
            database
                .begin_read()
                .unwrap()
                .list_tables()
                .unwrap()
                .count(),
            0
        );
    }
    let original = {
        let storage = RedbStorage::open(&unrelated).unwrap();
        storage.store().put(b"keep", b"foreign").unwrap();
        storage.record_store().unwrap().database_id()
    };
    assert!(RedbStorage::open_restored(
        &unrelated,
        request,
        VersionedSessionOptions::default(),
        &control
    )
    .is_err());
    let unchanged = RedbStorage::open(&unrelated).unwrap();
    assert_eq!(unchanged.record_store().unwrap().database_id(), original);
    assert_eq!(
        unchanged.store().get(b"keep").unwrap().as_deref(),
        Some(b"foreign".as_slice())
    );
}
