//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Source-independent image imports preserve actual artifacts, committed revisions and fresh history admission.

use std::{
    fs::{File, OpenOptions},
    path::Path,
};

use redb::ReadableDatabase;
use uqa_storage::{
    backup::{
        BackupError, BackupKey, BackupLimits, BackupRoot, LocalRepository, Protection, ProtectionId,
    },
    key_value::{
        conformance::{
            verify_diskann_restore_source, verify_diskann_restored, verify_diskann_restored_writes,
        },
        KeyValueNotificationRecords,
    },
    mvcc::{
        DatabaseRestore, IdentifierRequest, NotificationRecordLayout, VersionedPersistence,
        VersionedSessionOptions,
    },
    read_control::StorageReadControl,
    KeyValueStore, PersistentStorageProvider,
};

use crate::{RedbBackupImport, RedbBackupImportStep, RedbStorage};

fn file(path: &Path) -> File {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(path)
        .unwrap()
}

fn limits() -> BackupLimits {
    BackupLimits::new(4096, 4096).unwrap()
}

fn protection() -> Protection {
    Protection::Encrypted {
        domain: ProtectionId::from_bytes([15; 16]).unwrap(),
        key: BackupKey::new([37; 32]),
    }
}

pub(super) fn capture(
    owner: &RedbStorage,
    path: &Path,
    control: &StorageReadControl,
) -> (LocalRepository, BackupRoot) {
    let image = owner.capture_backup_image(control).unwrap();
    let repository = LocalRepository::create(path, protection(), limits(), control).unwrap();
    let root = {
        let mut operation = repository
            .begin_backup(image.cut(), None, 16 << 20, control)
            .unwrap();
        operation.prepare_image(image, 2 << 20, control).unwrap();
        operation.publish(control).unwrap()
    };
    (repository, root)
}

#[test]
fn redb_backup_import_restores_artifacts_revisions_and_allocators_without_the_source() {
    let directory = tempfile::tempdir().unwrap();
    let source_path = directory.path().join("source.redb");
    let target_path = directory.path().join("private-target.redb");
    let source = RedbStorage::open(&source_path).unwrap();
    let setup = StorageReadControl::with_limit(32 << 20);
    let session = source.open_session().unwrap();
    let generation = verify_diskann_restore_source(&*session.backend).unwrap();
    drop(session);
    let store = source.store();
    store.put(b"", b"").unwrap();
    store.put(b"large", &vec![193; (128 << 10) + 17]).unwrap();
    store.put(b"deleted", b"absent from the cut").unwrap();
    store.delete(b"deleted").unwrap();
    let notification = KeyValueNotificationRecords.key(&setup).unwrap();
    store.put(&notification, b"excluded delivery").unwrap();
    let records = source.record_store().unwrap();
    records
        .allocate_identifiers(b"user-sequence", IdentifierRequest::Observe(77), &setup)
        .unwrap();
    let old_transaction = records.allocate_transaction(&setup).unwrap();
    let expected = super::backup_image::expected_records(&source, b"\xffunused", &notification);
    let capture_control = StorageReadControl::with_limit(128 << 10);
    let repository_path = directory.path().join("backup");
    let (repository, root) = capture(&source, &repository_path, &capture_control);
    let request = DatabaseRestore::new(root.cut().source).unwrap();
    store.put(b"large", b"later source value").unwrap();
    records
        .allocate_identifiers(b"user-sequence", IdentifierRequest::Observe(99), &setup)
        .unwrap();
    drop((records, store, source, repository));
    std::fs::remove_file(&source_path).unwrap();

    let control = StorageReadControl::with_limit(384 << 10);
    let repository =
        LocalRepository::open(&repository_path, protection(), limits(), &control).unwrap();
    let lease = repository.open_backup(root.backup(), &control).unwrap();
    let mut import = RedbBackupImport::new(
        file(&target_path),
        &lease,
        request,
        256 << 10,
        2 << 20,
        &control,
    )
    .unwrap();
    assert!(
        RedbStorage::open(&target_path).is_err(),
        "import owns the exclusive file"
    );
    complete_import(&mut import);
    assert_eq!(import.imported_records(), expected.len() as u64);
    assert_eq!(control.memory().used(), 0);
    assert!(control.memory().peak() <= 384 << 10);
    control.cancellation().cancel();
    assert_eq!(import.advance().unwrap(), RedbBackupImportStep::Complete);

    // Successful import closes every physical handle before returning Complete.
    let restored = RedbStorage::open(&target_path).unwrap();
    let records = restored.record_store().unwrap();
    assert_eq!(records.database_id(), request.target());
    assert_eq!(
        records.snapshot(&setup).unwrap().sequence(),
        root.cut().sequence.unwrap()
    );
    assert_eq!(
        records
            .identifier_watermark(b"user-sequence", &setup)
            .unwrap(),
        Some(77)
    );
    assert!(matches!(
        records.commit_status(old_transaction, &setup),
        Err(uqa_storage::mvcc::VersionError::WrongDatabase)
    ));
    assert_eq!(
        super::backup_image::expected_records(&restored, b"\xffunused", &notification),
        expected
    );
    let session = restored.open_session().unwrap();
    verify_diskann_restored(&*session.backend, generation).unwrap();
    verify_diskann_restored_writes(&*session.backend, generation).unwrap();
    restored
        .store()
        .put(b"after-restore", b"must survive retry")
        .unwrap();
    let after = super::backup_image::expected_records(&restored, b"\xffunused", &notification);
    drop((session, records, restored));
    let retry = RedbStorage::open_restored(
        &target_path,
        request,
        VersionedSessionOptions::default(),
        &setup,
    )
    .unwrap();
    assert_eq!(
        retry.record_store().unwrap().database_id(),
        request.target()
    );
    assert_eq!(
        super::backup_image::expected_records(&retry, b"\xffunused", &notification),
        after
    );
}

fn complete_import(import: &mut RedbBackupImport<'_>) {
    loop {
        let before = import.imported_records() + import.imported_identifiers();
        let step = import.advance().unwrap();
        assert!(import.imported_records() + import.imported_identifiers() - before <= 1);
        if step == RedbBackupImportStep::Complete {
            break;
        }
    }
}

fn assert_unavailable(path: &Path) {
    assert!(
        RedbStorage::open(path).is_err(),
        "an incomplete image is not an ordinary database"
    );
    let database = redb::Database::open(path).unwrap();
    let transaction = database.begin_read().unwrap();
    let metadata = transaction.open_table(super::super::METADATA).unwrap();
    assert_eq!(
        super::super::codec::read_u64(&metadata, "format").unwrap(),
        0
    );
}

#[test]
fn redb_backup_import_drop_cancel_and_limits_leave_the_target_unavailable() {
    let directory = tempfile::tempdir().unwrap();
    let source = RedbStorage::open(directory.path().join("source")).unwrap();
    source.store().put(b"first", b"small").unwrap();
    source.store().put(b"large", &vec![71; 64 << 10]).unwrap();
    let setup = StorageReadControl::with_limit(128 << 10);
    let (repository, root) = capture(&source, &directory.path().join("backup"), &setup);
    let lease = repository.open_backup(root.backup(), &setup).unwrap();
    for failure in 0..7 {
        let target = directory.path().join(format!("incomplete-{failure}"));
        let control =
            StorageReadControl::with_limit(if failure == 6 { 32 << 10 } else { 128 << 10 });
        let maximum = if failure == 5 { 1024 } else { 128 << 10 };
        let request = DatabaseRestore::new(root.cut().source).unwrap();
        let mut import =
            RedbBackupImport::new(file(&target), &lease, request, maximum, 2 << 20, &control)
                .unwrap();
        if failure < 4 {
            for _ in 0..failure {
                assert_eq!(import.advance().unwrap(), RedbBackupImportStep::Pending);
            }
            drop(import);
        } else {
            import.advance().unwrap();
            import.advance().unwrap();
            if failure == 4 {
                control.cancellation().cancel();
            }
            let error = import.advance().unwrap_err();
            match failure {
                4 => assert!(matches!(error, BackupError::Cancelled(_))),
                5 => assert!(matches!(
                    error,
                    BackupError::Limit {
                        resource: "redb import value",
                        ..
                    }
                )),
                _ => assert!(matches!(error, BackupError::Memory(_))),
            }
            assert!(matches!(import.advance(), Err(BackupError::Finished)));
        }
        assert_eq!(control.memory().used(), 0);
        assert_unavailable(&target);
        assert!(RedbStorage::open_restored(
            &target,
            request,
            VersionedSessionOptions::default(),
            &setup
        )
        .is_err());
    }
}

#[test]
fn redb_backup_import_empty_image_preserves_sequence_and_never_reuses_a_populated_file() {
    let directory = tempfile::tempdir().unwrap();
    let source = RedbStorage::open(directory.path().join("source")).unwrap();
    source.store().put(b"removed", b"value").unwrap();
    source.store().delete(b"removed").unwrap();
    let control = StorageReadControl::with_limit(128 << 10);
    let (repository, root) = capture(&source, &directory.path().join("backup"), &control);
    let lease = repository.open_backup(root.backup(), &control).unwrap();
    let target = directory.path().join("target");
    let request = DatabaseRestore::new(root.cut().source).unwrap();
    let mut import =
        RedbBackupImport::new(file(&target), &lease, request, 1, 2 << 20, &control).unwrap();
    complete_import(&mut import);
    assert_eq!(import.imported_records(), 0);
    let restored = RedbStorage::open(&target).unwrap();
    let records = restored.record_store().unwrap();
    assert_eq!(
        records.snapshot(&control).unwrap().sequence(),
        root.cut().sequence.unwrap()
    );
    restored.store().put(b"later", b"preserved").unwrap();
    drop((records, restored));
    let existing = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&target)
        .unwrap();
    assert!(matches!(
        RedbBackupImport::new(existing, &lease, request, 1, 2 << 20, &control),
        Err(BackupError::Invalid(
            "redb import requires an empty private regular file"
        ))
    ));
    let restored = RedbStorage::open(&target).unwrap();
    assert_eq!(
        restored.store().get(b"later").unwrap(),
        Some(b"preserved".to_vec())
    );
}

#[test]
fn redb_backup_import_rejects_final_object_corruption_after_copying_all_units() {
    use std::io::{Read, Seek, SeekFrom, Write};
    let directory = tempfile::tempdir().unwrap();
    let source = RedbStorage::open(directory.path().join("source")).unwrap();
    source.store().put(b"row", b"value").unwrap();
    let control = StorageReadControl::with_limit(128 << 10);
    let repository_path = directory.path().join("backup");
    let (repository, root) = capture(&source, &repository_path, &control);
    let lease = repository.open_backup(root.backup(), &control).unwrap();
    let target = directory.path().join("target");
    let request = DatabaseRestore::new(root.cut().source).unwrap();
    let mut import =
        RedbBackupImport::new(file(&target), &lease, request, 1024, 2 << 20, &control).unwrap();
    while import.imported_records() == 0 {
        import.advance().unwrap();
    }
    // Every unit was copied but whole-object verification has not begun. Mutating the stored object must prevent activation.
    let object = std::fs::read_dir(repository_path.join("objects"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(object)
        .unwrap();
    let mut byte = [0];
    file.read_exact(&mut byte).unwrap();
    byte[0] ^= 128;
    file.seek(SeekFrom::Start(0)).unwrap();
    file.write_all(&byte).unwrap();
    file.sync_all().unwrap();
    loop {
        match import.advance() {
            Ok(RedbBackupImportStep::Pending) => (),
            Ok(RedbBackupImportStep::Complete) => panic!("corrupt object activated"),
            Err(_) => break,
        }
    }
    assert_unavailable(&target);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn redb_backup_import_synchronization_failure_cannot_expose_the_source_history() {
    use std::sync::{atomic::Ordering, Arc};
    use uqa_storage::backup::ImageProvider;
    use uqa_storage::key_value::KeyValueImageImportReader;
    let directory = tempfile::tempdir().unwrap();
    let source = RedbStorage::open(directory.path().join("source")).unwrap();
    source.store().put(b"row", b"value").unwrap();
    let control = StorageReadControl::with_limit(128 << 10);
    let (repository, root) = capture(&source, &directory.path().join("backup"), &control);
    let lease = repository.open_backup(root.backup(), &control).unwrap();
    let request = DatabaseRestore::new(root.cut().source).unwrap();
    let backend = super::FaultBackend::default();
    let database = redb::Database::builder()
        .create_with_backend(backend.clone())
        .unwrap();
    let input =
        KeyValueImageImportReader::new(&lease, ImageProvider::Redb, 2 << 20, &control).unwrap();
    let mut import =
        RedbBackupImport::from_image(database, input, request, 1024, &control).unwrap();
    while import.imported_records() == 0 {
        import.advance().unwrap();
    }
    backend.fail_sync.store(true, Ordering::Relaxed);
    loop {
        match import.advance() {
            Ok(RedbBackupImportStep::Pending) => (),
            Ok(RedbBackupImportStep::Complete) => {
                panic!("synchronization failure was not reported")
            }
            Err(_) => break,
        }
    }
    assert!(!backend.fail_sync.load(Ordering::Relaxed));
    assert!(matches!(import.advance(), Err(BackupError::Finished)));
    assert_eq!(control.memory().used(), 0);
    let database = redb::Database::builder()
        .create_with_backend(backend.clone())
        .unwrap();
    let (format, identity) = {
        let transaction = database.begin_read().unwrap();
        let metadata = transaction.open_table(super::super::METADATA).unwrap();
        (
            super::super::codec::read_u64(&metadata, "format").unwrap(),
            super::super::codec::database_id(&metadata).unwrap(),
        )
    };
    match format {
        0 => {
            assert_eq!(identity, request.source());
            assert!(crate::RedbRecordStore::new(Arc::new(database)).is_err());
        }
        super::super::RECORD_FORMAT => {
            assert_eq!(identity, request.target());
            let restored = super::super::restore::open(database, request, &control).unwrap();
            assert_eq!(restored.database_id(), request.target());
            assert_eq!(
                restored
                    .snapshot(&control)
                    .unwrap()
                    .get(b"row", &control)
                    .unwrap()
                    .unwrap()
                    .value()
                    .map(|v| &***v),
                Some(b"value".as_slice())
            );
        }
        _ => panic!("partial format transition"),
    }
}

#[test]
fn redb_backup_import_rejects_invalid_requests_before_initializing_the_target() {
    use uqa_storage::mvcc::DatabaseId;
    let directory = tempfile::tempdir().unwrap();
    let source = RedbStorage::open(directory.path().join("source")).unwrap();
    let control = StorageReadControl::with_limit(128 << 10);
    let (repository, root) = capture(&source, &directory.path().join("backup"), &control);
    let lease = repository.open_backup(root.backup(), &control).unwrap();
    let request = DatabaseRestore::new(root.cut().source).unwrap();
    let mut foreign = root.cut().source.as_bytes();
    foreign[0] ^= 1;
    for (index, request, maximum) in [
        (0, request, 0),
        (
            1,
            DatabaseRestore::new(DatabaseId::from_bytes(foreign)).unwrap(),
            1024,
        ),
        (
            2,
            DatabaseRestore::from_identities(root.cut().source, DatabaseId::from_bytes([0; 16]))
                .unwrap(),
            1024,
        ),
    ] {
        let path = directory.path().join(format!("target-{index}"));
        assert!(matches!(
            RedbBackupImport::new(file(&path), &lease, request, maximum, 2 << 20, &control),
            Err(BackupError::Invalid(_))
        ));
        assert_eq!(std::fs::metadata(path).unwrap().len(), 0);
        assert_eq!(control.memory().used(), 0);
    }
}
