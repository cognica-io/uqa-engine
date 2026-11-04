//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn sqlite_native_incremental_backup_restores_membership_and_artifacts_in_every_file_mode() {
    verify_incremental_restore(false);
}

#[test]
fn sqlite_native_standalone_copy_restores_incremental_images_in_every_file_mode() {
    verify_incremental_restore(true);
}

fn verify_incremental_restore(standalone: bool) {
    for mode in 0..4 {
        let directory = tempfile::tempdir().unwrap();
        let source_path = directory.path().join("source");
        std::fs::create_dir(&source_path).unwrap();
        let connection = open(&source_path.join("database"), mode).unwrap();
        let provider = SQLiteStorageProvider::new(connection.clone());
        let session = provider.open_session().unwrap();
        let generation = verify_diskann_restore_source(&*session.backend).unwrap();
        session.catalog.set_metadata("keep", "unchanged").unwrap();
        session.catalog.set_metadata("change", "before").unwrap();
        session.catalog.set_metadata("delete", "removed").unwrap();
        let control = StorageReadControl::with_limit(1 << 20);
        let (repository, parent) = capture(&provider, &directory.path().join("backup"), &control);
        session.catalog.set_metadata("change", "after!").unwrap();
        session.catalog.set_metadata("new", "added").unwrap();
        session.catalog.delete_metadata("delete").unwrap();
        let records = SQLiteRecordStore::for_native(&connection, &control).unwrap();
        records
            .allocate_identifiers(b"incremental-ids", IdentifierRequest::Observe(93), &control)
            .unwrap();
        let expected = rows(&records, &control);
        let image = provider.capture_backup_image(&control).unwrap();
        session
            .catalog
            .set_metadata("change", "after the cut")
            .unwrap();
        let mut operation = repository
            .begin_backup(image.cut(), Some(parent.backup()), 16 << 20, &control)
            .unwrap();
        let report = operation.prepare_image(image, 2 << 20, &control).unwrap();
        assert!(report.image.reused_units > 0);
        assert!(report.image.written_units > 0);
        assert_eq!(report.image.parent_units, parent.summary().units);
        assert_eq!(
            report.image.hashed_payload_bytes,
            report.inventory.decoded_bytes
        );
        assert_eq!(
            report.source.copied_payload_bytes + 56 + 8 * expected.len() as u64,
            report.inventory.decoded_bytes
        );
        let child = operation.publish(&control).unwrap();
        drop((operation, records, session, provider, connection));
        std::fs::remove_dir_all(source_path).unwrap();

        let (repository, child) = if standalone {
            copy_repository(repository, child, directory.path(), &control)
        } else {
            (repository, child)
        };
        let lease = repository.open_backup(child.backup(), &control).unwrap();
        let request = DatabaseRestore::new(child.cut().source).unwrap();
        let target = directory.path().join("target");
        empty(&target);
        let mut import =
            SQLiteNativeBackupImport::new(&target, &lease, request, options(mode), &control)
                .unwrap();
        complete(&mut import);
        let connection = open(&target, mode).unwrap();
        let records = SQLiteRecordStore::for_native(&connection, &control).unwrap();
        assert_eq!(rows(&records, &control), expected);
        assert_eq!(records.database_id(), request.target());
        assert_eq!(
            records
                .identifier_watermark(b"incremental-ids", &control)
                .unwrap(),
            Some(93)
        );
        let provider = SQLiteStorageProvider::new(connection);
        let session = provider.open_session().unwrap();
        verify_diskann_restored(&*session.backend, generation).unwrap();
        verify_diskann_restored_writes(&*session.backend, generation).unwrap();
        session
            .catalog
            .set_metadata("after-restore", "writable")
            .unwrap();
        assert_eq!(control.memory().used(), 0);
    }
}

fn copy_repository(
    repository: uqa_storage::backup::LocalRepository,
    child: uqa_storage::backup::BackupRoot,
    directory: &std::path::Path,
    control: &StorageReadControl,
) -> (
    uqa_storage::backup::LocalRepository,
    uqa_storage::backup::BackupRoot,
) {
    let source = repository.open_backup(child.backup(), control).unwrap();
    let copied_repository = uqa_storage::backup::LocalRepository::create(
        directory.join("copy"),
        uqa_storage::backup::Protection::Encrypted {
            domain: uqa_storage::backup::ProtectionId::from_bytes([93; 16]).unwrap(),
            key: uqa_storage::backup::BackupKey::new([94; 32]),
        },
        uqa_storage::backup::BackupLimits::new(4096, 4096).unwrap(),
        control,
    )
    .unwrap();
    let mut copy = copied_repository
        .begin_backup(child.cut(), None, 16 << 20, control)
        .unwrap();
    let report = copy.prepare_copy(&source, 2 << 20, control).unwrap();
    assert_eq!(report.inventory, child.summary());
    assert_eq!(report.copied_payload_bytes, child.summary().decoded_bytes);
    let copied = copy.publish(control).unwrap();
    assert_eq!(copied.cut(), child.cut());
    assert_eq!(copied.parent(), None);
    drop((copy, source));
    drop(repository);
    std::fs::remove_dir_all(directory.join("backup")).unwrap();
    (copied_repository, copied)
}
