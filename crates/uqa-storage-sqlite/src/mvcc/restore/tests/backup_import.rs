//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Actual image import, unavailable partial files and history recovery across all four file modes.

use std::{collections::BTreeMap, fs::OpenOptions, path::Path};

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
        DatabaseRestore, IdentifierRequest, NotificationRecordLayout, VersionError,
        VersionedPersistence,
    },
    read_control::StorageReadControl,
    KeyValueStore, StorageEncryptionKey,
};

use super::{control, open, restored};
use crate::{
    mvcc::{SQLiteBackupImportOptions, SQLiteBackupImportStep, SQLiteKeyValueBackupImport},
    ManagedConnection, SQLiteCompressionOptions, SQLiteKeyValueStorage, SQLiteRecordStore,
};

fn options(mode: usize) -> SQLiteBackupImportOptions {
    SQLiteBackupImportOptions {
        compression: (mode >= 2).then(SQLiteCompressionOptions::default),
        encryption_key: (mode % 2 == 1).then(|| StorageEncryptionKey::new("closed-backup")),
        maximum_value_bytes: 1 << 20,
        maximum_workspace_bytes: 2 << 20,
    }
}

fn empty_file(path: &Path) {
    drop(
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .unwrap(),
    );
}

fn limits() -> BackupLimits {
    BackupLimits::new(4096, 4096).unwrap()
}
fn protection() -> Protection {
    Protection::Encrypted {
        domain: ProtectionId::from_bytes([29; 16]).unwrap(),
        key: BackupKey::new([41; 32]),
    }
}

fn capture(
    owner: &SQLiteKeyValueStorage,
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

fn expected(records: &SQLiteRecordStore, excluded: &[u8]) -> BTreeMap<Vec<u8>, (u64, Vec<u8>)> {
    let control = control();
    let snapshot = records.snapshot(&control).unwrap();
    let mut expected = BTreeMap::new();
    snapshot
        .visit_prefix(b"", None, usize::MAX, &control, &mut |key, record| {
            if key != excluded {
                if let Some(value) = record.value {
                    expected.insert(
                        key.to_vec(),
                        (record.revision.unwrap().as_u64(), value.to_vec()),
                    );
                }
            }
            Ok(true)
        })
        .unwrap();
    expected
}

fn complete(import: &mut SQLiteKeyValueBackupImport<'_>) {
    loop {
        let before = import.imported_records() + import.imported_identifiers();
        let step = import.advance().unwrap();
        assert!(import.imported_records() + import.imported_identifiers() - before <= 1);
        if step == SQLiteBackupImportStep::Complete {
            break;
        }
    }
}

fn unavailable(path: &Path, mode: usize, request: DatabaseRestore) {
    assert!(
        open(path, mode).is_err(),
        "partial image must reject ordinary open"
    );
    assert!(
        restored(path, mode, request, &control()).is_err(),
        "partial image must reject history activation"
    );
}

#[test]
fn sqlite_backup_import_preserves_artifacts_revisions_and_allocators_after_source_loss() {
    for mode in 0..4 {
        let directory = tempfile::tempdir().unwrap();
        let source_directory = directory.path().join("source");
        std::fs::create_dir(&source_directory).unwrap();
        let source_path = source_directory.join("database");
        let target_path = directory.path().join("target");
        let connection = open(&source_path, (mode + 1) % 4).unwrap();
        let owner = SQLiteKeyValueStorage::from_connection(connection.clone()).unwrap();
        let generation = verify_diskann_restore_source(&owner.backend()).unwrap();
        let setup = control();
        let store = owner.store();
        store.put(b"", b"").unwrap();
        store.put(b"large", &vec![193; (512 << 10) + 17]).unwrap();
        store.put(b"deleted", b"gone").unwrap();
        store.delete(b"deleted").unwrap();
        let notification = KeyValueNotificationRecords.key(&setup).unwrap();
        store.put(&notification, b"delivery excluded").unwrap();
        let records = SQLiteRecordStore::for_key_value(&connection, &setup).unwrap();
        records
            .allocate_identifiers(b"user-sequence", IdentifierRequest::Observe(77), &setup)
            .unwrap();
        let old_transaction = records.allocate_transaction(&setup).unwrap();
        let wanted = expected(&records, &notification);
        let repository_path = directory.path().join("backup");
        let (repository, root) = capture(
            &owner,
            &repository_path,
            &StorageReadControl::with_limit(128 << 10),
        );
        let request = DatabaseRestore::new(root.cut().source).unwrap();
        store.put(b"large", b"later source value").unwrap();
        records
            .allocate_identifiers(b"user-sequence", IdentifierRequest::Observe(99), &setup)
            .unwrap();
        drop((repository, records, store, owner, connection));
        std::fs::remove_dir_all(source_directory).unwrap();

        // The value exceeds the complete UQA buffer allowance; only bounded chunks may be retained.
        let control = StorageReadControl::with_limit(128 << 10);
        let repository =
            LocalRepository::open(&repository_path, protection(), limits(), &control).unwrap();
        let lease = repository.open_backup(root.backup(), &control).unwrap();
        empty_file(&target_path);
        let mut import =
            SQLiteKeyValueBackupImport::new(&target_path, &lease, request, options(mode), &control)
                .unwrap();
        let waiting = StorageReadControl::with_limit(128 << 10);
        let _deadline = waiting.cancellation().deadline(
            std::time::Duration::from_millis(20),
            uqa_core::CancellationReason::UserRequest,
        );
        assert!(matches!(
            restored(&target_path, mode, request, &waiting),
            Err(crate::SQLiteError::Cancelled(_))
        ));
        complete(&mut import);
        assert_eq!(import.imported_records(), wanted.len() as u64);
        assert_eq!(control.memory().used(), 0);
        assert!(control.memory().peak() <= 128 << 10);
        control.cancellation().cancel();
        assert_eq!(import.advance().unwrap(), SQLiteBackupImportStep::Complete);

        let connection = open(&target_path, mode).unwrap();
        let records = SQLiteRecordStore::for_key_value(&connection, &setup).unwrap();
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
            Err(VersionError::WrongDatabase)
        ));
        assert_eq!(expected(&records, &notification), wanted);
        let owner = SQLiteKeyValueStorage::from_connection(connection.clone()).unwrap();
        verify_diskann_restored(&owner.backend(), generation).unwrap();
        verify_diskann_restored_writes(&owner.backend(), generation).unwrap();
        owner
            .store()
            .put(b"later", b"preserved after retry")
            .unwrap();
        let after = expected(&records, &notification);
        drop((owner, records, connection));
        let retry = restored(&target_path, mode, request, &setup).unwrap();
        let records = SQLiteRecordStore::for_key_value(&retry, &setup).unwrap();
        assert_eq!(expected(&records, &notification), after);
    }
}

#[test]
fn sqlite_backup_import_drop_cancellation_and_limits_keep_partial_files_unavailable() {
    let directory = tempfile::tempdir().unwrap();
    let owner =
        SQLiteKeyValueStorage::from_connection(ManagedConnection::open_in_memory().unwrap())
            .unwrap();
    owner.store().put(b"first", b"small").unwrap();
    owner.store().put(b"large", &vec![71; 64 << 10]).unwrap();
    let setup = control();
    let (repository, root) = capture(&owner, &directory.path().join("backup"), &setup);
    let lease = repository.open_backup(root.backup(), &setup).unwrap();
    for mode in 0..4 {
        for failure in 0..6 {
            let target = directory
                .path()
                .join(format!("incomplete-{mode}-{failure}"));
            empty_file(&target);
            let control = StorageReadControl::with_limit(128 << 10);
            let request = DatabaseRestore::new(root.cut().source).unwrap();
            let mut options = options(mode);
            if failure == 4 {
                options.maximum_value_bytes = 1024;
            }
            let mut import =
                SQLiteKeyValueBackupImport::new(&target, &lease, request, options, &control)
                    .unwrap();
            let occupied = (failure == 5).then(|| {
                control
                    .memory()
                    .reserve((128 << 10) - control.memory().used())
                    .unwrap()
            });
            if failure < 3 {
                for _ in 0..failure {
                    assert_eq!(import.advance().unwrap(), SQLiteBackupImportStep::Pending);
                }
                drop(import);
            } else {
                if failure == 3 {
                    control.cancellation().cancel();
                }
                let error = loop {
                    match import.advance() {
                        Ok(SQLiteBackupImportStep::Pending) => (),
                        Ok(SQLiteBackupImportStep::Complete) => panic!("failed image completed"),
                        Err(error) => break error,
                    }
                };
                match failure {
                    3 => assert!(matches!(error, BackupError::Cancelled(_))),
                    4 => assert!(matches!(
                        error,
                        BackupError::Limit {
                            resource: "SQLite import value",
                            ..
                        }
                    )),
                    _ => assert!(matches!(error, BackupError::Memory(_)), "{error:?}"),
                }
                assert!(matches!(import.advance(), Err(BackupError::Finished)));
                drop(import);
            }
            drop(occupied);
            assert_eq!(control.memory().used(), 0);
            unavailable(&target, mode, request);
        }
    }
}

#[test]
fn sqlite_backup_import_empty_image_preserves_sequence_and_refuses_populated_files() {
    let directory = tempfile::tempdir().unwrap();
    let owner =
        SQLiteKeyValueStorage::from_connection(ManagedConnection::open_in_memory().unwrap())
            .unwrap();
    owner.store().put(b"removed", b"value").unwrap();
    owner.store().delete(b"removed").unwrap();
    let setup = control();
    let (repository, root) = capture(&owner, &directory.path().join("backup"), &setup);
    let lease = repository.open_backup(root.backup(), &setup).unwrap();
    for mode in 0..4 {
        let target = directory.path().join(format!("empty-{mode}"));
        empty_file(&target);
        let request = DatabaseRestore::new(root.cut().source).unwrap();
        let mut import =
            SQLiteKeyValueBackupImport::new(&target, &lease, request, options(mode), &setup)
                .unwrap();
        complete(&mut import);
        assert_eq!(import.imported_records(), 0);
        let connection = open(&target, mode).unwrap();
        let records = SQLiteRecordStore::for_key_value(&connection, &setup).unwrap();
        assert_eq!(
            records.snapshot(&setup).unwrap().sequence(),
            root.cut().sequence.unwrap()
        );
        let owner = SQLiteKeyValueStorage::from_connection(connection.clone()).unwrap();
        owner.store().put(b"later", b"preserved").unwrap();
        drop((owner, records, connection));
        assert!(
            SQLiteKeyValueBackupImport::new(&target, &lease, request, options(mode), &setup)
                .is_err()
        );
        let owner = SQLiteKeyValueStorage::from_connection(open(&target, mode).unwrap()).unwrap();
        assert_eq!(
            owner.store().get(b"later").unwrap(),
            Some(b"preserved".to_vec())
        );
    }
}

#[test]
fn sqlite_backup_import_preserves_every_source_and_destination_protection_combination() {
    for source_mode in 0..4 {
        let directory = tempfile::tempdir().unwrap();
        let source_directory = directory.path().join("source");
        std::fs::create_dir(&source_directory).unwrap();
        let connection = open(&source_directory.join("database"), source_mode).unwrap();
        let owner = SQLiteKeyValueStorage::from_connection(connection.clone()).unwrap();
        owner.store().put(b"", b"").unwrap();
        owner.store().put(b"opaque", &[0, 255, 128, 0, 42]).unwrap();
        let setup = control();
        let records = SQLiteRecordStore::for_key_value(&connection, &setup).unwrap();
        records
            .allocate_identifiers(b"sequence", IdentifierRequest::Observe(57), &setup)
            .unwrap();
        let wanted = expected(&records, b"absent");
        let (repository, root) = capture(&owner, &directory.path().join("backup"), &setup);
        drop((records, owner, connection));
        std::fs::remove_dir_all(source_directory).unwrap();
        let lease = repository.open_backup(root.backup(), &setup).unwrap();
        for target_mode in 0..4 {
            let target = directory.path().join(format!("target-{target_mode}"));
            empty_file(&target);
            let request = DatabaseRestore::new(root.cut().source).unwrap();
            let mut import = SQLiteKeyValueBackupImport::new(
                &target,
                &lease,
                request,
                options(target_mode),
                &setup,
            )
            .unwrap();
            complete(&mut import);
            let connection = open(&target, target_mode).unwrap();
            let records = SQLiteRecordStore::for_key_value(&connection, &setup).unwrap();
            assert_eq!(records.database_id(), request.target());
            assert_eq!(expected(&records, b"absent"), wanted);
            assert_eq!(
                records.identifier_watermark(b"sequence", &setup).unwrap(),
                Some(57)
            );
        }
    }
}

#[test]
fn sqlite_backup_import_verifies_whole_objects_after_copying_every_record() {
    use std::io::{Read, Seek, SeekFrom, Write};
    for mode in 0..4 {
        let directory = tempfile::tempdir().unwrap();
        let owner =
            SQLiteKeyValueStorage::from_connection(ManagedConnection::open_in_memory().unwrap())
                .unwrap();
        owner.store().put(b"row", b"value").unwrap();
        let setup = control();
        let repository_path = directory.path().join("backup");
        let (repository, root) = capture(&owner, &repository_path, &setup);
        let lease = repository.open_backup(root.backup(), &setup).unwrap();
        let target = directory.path().join("target");
        empty_file(&target);
        let request = DatabaseRestore::new(root.cut().source).unwrap();
        let mut import =
            SQLiteKeyValueBackupImport::new(&target, &lease, request, options(mode), &setup)
                .unwrap();
        while import.imported_records() == 0 {
            import.advance().unwrap();
        }
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
                Ok(SQLiteBackupImportStep::Pending) => (),
                Ok(SQLiteBackupImportStep::Complete) => panic!("corrupt object activated"),
                Err(_) => break,
            }
        }
        unavailable(&target, mode, request);
    }
}

#[test]
fn sqlite_backup_import_uncertain_history_publication_resumes_the_original_request() {
    for mode in 0..4 {
        for boundary in [
            super::Boundary::IntentPublished,
            super::Boundary::CoordinatorPublished,
            super::Boundary::Completed,
        ] {
            let directory = tempfile::tempdir().unwrap();
            let owner = SQLiteKeyValueStorage::from_connection(
                ManagedConnection::open_in_memory().unwrap(),
            )
            .unwrap();
            owner.store().put(b"row", b"original").unwrap();
            let setup = control();
            let (repository, root) = capture(&owner, &directory.path().join("backup"), &setup);
            let lease = repository.open_backup(root.backup(), &setup).unwrap();
            let target = directory.path().join("target");
            empty_file(&target);
            let request = DatabaseRestore::new(root.cut().source).unwrap();
            let mut import =
                SQLiteKeyValueBackupImport::new(&target, &lease, request, options(mode), &setup)
                    .unwrap();
            let _injection = super::inject(boundary, || {
                Err(VersionError::InvalidEncoding("lost history publication reply").into())
            });
            loop {
                match import.advance() {
                    Ok(SQLiteBackupImportStep::Pending) => (),
                    Ok(SQLiteBackupImportStep::Complete) => {
                        panic!("lost reply must remain uncertain")
                    }
                    Err(error) => {
                        assert!(error.to_string().contains("lost history publication reply"));
                        break;
                    }
                }
            }
            if boundary != super::Boundary::Completed {
                assert!(open(&target, mode).is_err());
            }
            let connection = restored(&target, mode, request, &setup).unwrap();
            let records = SQLiteRecordStore::for_key_value(&connection, &setup).unwrap();
            assert_eq!(records.database_id(), request.target());
            let owner = SQLiteKeyValueStorage::from_connection(connection.clone()).unwrap();
            assert_eq!(
                owner.store().get(b"row").unwrap(),
                Some(b"original".to_vec())
            );
            owner.store().put(b"later", b"preserved").unwrap();
            drop((owner, records, connection));
            let owner = SQLiteKeyValueStorage::from_connection(
                restored(&target, mode, request, &setup).unwrap(),
            )
            .unwrap();
            assert_eq!(
                owner.store().get(b"later").unwrap(),
                Some(b"preserved".to_vec())
            );
        }
    }
}

#[test]
fn sqlite_backup_import_invalid_options_leave_the_empty_destination_untouched() {
    let directory = tempfile::tempdir().unwrap();
    let owner =
        SQLiteKeyValueStorage::from_connection(ManagedConnection::open_in_memory().unwrap())
            .unwrap();
    let setup = control();
    let (repository, root) = capture(&owner, &directory.path().join("backup"), &setup);
    let lease = repository.open_backup(root.backup(), &setup).unwrap();
    let request = DatabaseRestore::new(root.cut().source).unwrap();
    for failure in 0..4 {
        let target = directory.path().join(format!("invalid-{failure}"));
        empty_file(&target);
        let mut options = options(0);
        let mut request = request;
        match failure {
            0 => options.maximum_value_bytes = 0,
            1 => options.encryption_key = Some(StorageEncryptionKey::new("")),
            2 => {
                options.compression = Some(SQLiteCompressionOptions {
                    page_size: 3,
                    ..SQLiteCompressionOptions::default()
                });
            }
            _ => {
                request = DatabaseRestore::new(uqa_storage::mvcc::DatabaseId::from_bytes([87; 16]))
                    .unwrap();
            }
        }
        assert!(
            SQLiteKeyValueBackupImport::new(&target, &lease, request, options, &setup).is_err()
        );
        assert_eq!(target.metadata().unwrap().len(), 0);
    }
}

#[test]
fn sqlite_backup_import_exhausted_verification_spill_keeps_the_target_unavailable() {
    let directory = tempfile::tempdir().unwrap();
    let owner =
        SQLiteKeyValueStorage::from_connection(ManagedConnection::open_in_memory().unwrap())
            .unwrap();
    for index in 0_u32..512 {
        owner.store().put(&index.to_be_bytes(), b"value").unwrap();
    }
    let setup = control();
    let (repository, root) = capture(&owner, &directory.path().join("backup"), &setup);
    let lease = repository.open_backup(root.backup(), &setup).unwrap();
    for mode in 0..4 {
        let target = directory.path().join(format!("limited-{mode}"));
        empty_file(&target);
        let request = DatabaseRestore::new(root.cut().source).unwrap();
        let mut options = options(mode);
        options.maximum_workspace_bytes = 0;
        let control = StorageReadControl::with_limit(128 << 10);
        let mut import =
            SQLiteKeyValueBackupImport::new(&target, &lease, request, options, &control).unwrap();
        loop {
            match import.advance() {
                Ok(SQLiteBackupImportStep::Pending) => (),
                Ok(SQLiteBackupImportStep::Complete) => {
                    panic!("verification exceeded its spill allowance")
                }
                Err(error) => {
                    assert!(matches!(error, BackupError::Limit { .. }), "{error:?}");
                    break;
                }
            }
        }
        assert!(import.imported_records() > 0);
        assert!(matches!(import.advance(), Err(BackupError::Finished)));
        assert_eq!(control.memory().used(), 0);
        unavailable(&target, mode, request);
    }
}

#[test]
fn sqlite_backup_import_process_exit_keeps_partial_images_unavailable() {
    use uqa_storage::mvcc::DatabaseId;
    const ENV: &str = "UQA_SQLITE_IMAGE_EXIT_DIR";
    if let Some(directory) = std::env::var_os(ENV) {
        let directory = Path::new(&directory);
        let mode = std::fs::read(directory.join("mode")).unwrap()[0] as usize;
        let root = BackupRoot::decode(&std::fs::read(directory.join("root")).unwrap()).unwrap();
        let target = DatabaseId::from_bytes(
            std::fs::read(directory.join("request"))
                .unwrap()
                .try_into()
                .unwrap(),
        );
        let request = DatabaseRestore::from_identities(root.cut().source, target).unwrap();
        let control = control();
        let repository =
            LocalRepository::open(directory.join("backup"), protection(), limits(), &control)
                .unwrap();
        let lease = repository.open_backup(root.backup(), &control).unwrap();
        let mut import = SQLiteKeyValueBackupImport::new(
            &directory.join("target"),
            &lease,
            request,
            options(mode),
            &control,
        )
        .unwrap();
        while import.imported_records() == 0 {
            import.advance().unwrap();
        }
        std::process::exit(79);
    }
    for mode in 0..4 {
        let directory = tempfile::tempdir().unwrap();
        let owner =
            SQLiteKeyValueStorage::from_connection(ManagedConnection::open_in_memory().unwrap())
                .unwrap();
        owner.store().put(b"first", b"retained prefix").unwrap();
        owner.store().put(b"last", b"not imported").unwrap();
        let (repository, root) = capture(&owner, &directory.path().join("backup"), &control());
        drop(repository);
        let request = DatabaseRestore::new(root.cut().source).unwrap();
        std::fs::write(directory.path().join("mode"), [mode as u8]).unwrap();
        std::fs::write(directory.path().join("root"), root.encode()).unwrap();
        std::fs::write(
            directory.path().join("request"),
            request.target().as_bytes(),
        )
        .unwrap();
        let target = directory.path().join("target");
        empty_file(&target);
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "mvcc::restore::tests::backup_import::sqlite_backup_import_process_exit_keeps_partial_images_unavailable", "--nocapture"])
            .env(ENV, directory.path()).output().unwrap();
        assert_eq!(
            child.status.code(),
            Some(79),
            "{}",
            String::from_utf8_lossy(&child.stderr)
        );
        unavailable(&target, mode, request);
    }
}
