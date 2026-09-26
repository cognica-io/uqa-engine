//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_storage::{
    key_value::conformance::{
        diskann_restore_records, verify_diskann_restore_source, verify_diskann_restored,
        verify_diskann_restored_rebuild, verify_diskann_restored_writes,
    },
    mvcc::{DatabaseRestore, VersionedPersistence, VersionedSessionOptions},
    read_control::StorageReadControl,
    PersistentStorageProvider,
};

use super::super::{FaultBackend, RedbRecordStore};
use std::sync::{atomic::Ordering, Arc};
use uqa_storage::{mvcc::VersionedKeyValueStore, KeyValueStorageBackend};

#[test]
fn diskann_backup_restore_preserves_records_changes_and_new_history_writes() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("diskann-source.redb");
    let copy = directory.path().join("diskann-copy.redb");
    let (generation, request, image) = {
        let owner = crate::RedbStorage::open(&path).unwrap();
        let session = owner.open_session().unwrap();
        let generation = verify_diskann_restore_source(&*session.backend).unwrap();
        let records = owner.record_store().unwrap();
        (
            generation,
            DatabaseRestore::new(records.database_id()).unwrap(),
            diskann_restore_records(&records).unwrap(),
        )
    };
    std::fs::copy(&path, &copy).unwrap();
    let control = StorageReadControl::with_limit(1 << 24);
    let options = VersionedSessionOptions::default();
    let (next, updated) = {
        let owner = crate::RedbStorage::open_restored(&copy, request, options, &control).unwrap();
        let records = owner.record_store().unwrap();
        assert_eq!(records.database_id(), request.target());
        assert_eq!(diskann_restore_records(&records).unwrap(), image);
        let session = owner.open_session().unwrap();
        verify_diskann_restored(&*session.backend, generation).unwrap();
        assert_eq!(diskann_restore_records(&records).unwrap(), image);
        let next = verify_diskann_restored_writes(&*session.backend, generation).unwrap();
        (next, diskann_restore_records(&records).unwrap())
    };
    {
        let retry = crate::RedbStorage::open_restored(&copy, request, options, &control).unwrap();
        let records = retry.record_store().unwrap();
        assert_eq!(records.database_id(), request.target());
        assert_eq!(diskann_restore_records(&records).unwrap(), updated);
        let session = retry.open_session().unwrap();
        verify_diskann_restored_rebuild(&*session.backend, next).unwrap();
    }
    {
        let reopened = crate::RedbStorage::open(&copy).unwrap();
        assert_eq!(
            diskann_restore_records(&reopened.record_store().unwrap()).unwrap(),
            updated
        );
        let session = reopened.open_session().unwrap();
        verify_diskann_restored_rebuild(&*session.backend, next).unwrap();
    }
    let original = crate::RedbStorage::open(&path).unwrap();
    let records = original.record_store().unwrap();
    assert_eq!(records.database_id(), request.source());
    assert_eq!(diskann_restore_records(&records).unwrap(), image);
    let session = original.open_session().unwrap();
    verify_diskann_restored(&*session.backend, generation).unwrap();
}

#[test]
fn diskann_backup_restore_resolves_failed_synchronization_without_losing_artifacts() {
    let physical = FaultBackend::default();
    let control = StorageReadControl::with_limit(1 << 24);
    let logical = |records: &RedbRecordStore| {
        records.migrate_key_value().unwrap();
        KeyValueStorageBackend::new(Arc::new(VersionedKeyValueStore::new(
            Arc::new(records.clone()),
            None,
            VersionedSessionOptions::default(),
        )))
    };
    let (generation, request, image) = {
        let database = redb::Database::builder()
            .create_with_backend(physical.clone())
            .unwrap();
        let records = RedbRecordStore::new(Arc::new(database)).unwrap();
        let generation = verify_diskann_restore_source(&logical(&records)).unwrap();
        let request = DatabaseRestore::new(records.database_id()).unwrap();
        let image = diskann_restore_records(&records).unwrap();
        physical.fail_sync.store(true, Ordering::Relaxed);
        assert!(crate::mvcc::restore::publish(&records, request, &control).is_err());
        assert!(!physical.fail_sync.load(Ordering::Relaxed));
        (generation, request, image)
    };
    {
        let database = redb::Database::builder()
            .create_with_backend(physical.clone())
            .unwrap();
        let records = RedbRecordStore::new(Arc::new(database)).unwrap();
        request.needs_restore(records.database_id()).unwrap();
        assert_eq!(diskann_restore_records(&records).unwrap(), image);
        verify_diskann_restored(&logical(&records), generation).unwrap();
    }
    let database = redb::Database::builder()
        .create_with_backend(physical)
        .unwrap();
    let records = crate::mvcc::restore::open(database, request, &control).unwrap();
    assert_eq!(records.database_id(), request.target());
    assert_eq!(diskann_restore_records(&records).unwrap(), image);
    verify_diskann_restored_writes(&logical(&records), generation).unwrap();
}
