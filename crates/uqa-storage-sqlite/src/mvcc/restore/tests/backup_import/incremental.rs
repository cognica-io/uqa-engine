//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn sqlite_kv_incremental_backup_restores_membership_and_artifacts_in_every_file_mode() {
    verify_incremental_restore(false);
}

#[test]
fn sqlite_kv_standalone_copy_restores_incremental_images_in_every_file_mode() {
    verify_incremental_restore(true);
}

fn verify_incremental_restore(standalone: bool) {
    for mode in 0..4 {
        let directory = tempfile::tempdir().unwrap();
        let source_path = directory.path().join("source");
        std::fs::create_dir(&source_path).unwrap();
        let connection = open(&source_path.join("database"), mode).unwrap();
        let owner = SQLiteKeyValueStorage::from_connection(connection.clone()).unwrap();
        let generation = verify_diskann_restore_source(&owner.backend()).unwrap();
        let control = StorageReadControl::with_limit(1 << 20);
        let store = owner.store();
        store.put(b"keep", b"unchanged").unwrap();
        store.put(b"change", b"before").unwrap();
        store.put(b"delete", b"removed").unwrap();
        let (repository, parent) = capture(&owner, &directory.path().join("backup"), &control);
        store.put(b"change", b"after!").unwrap();
        store.put(b"new", b"added").unwrap();
        store.delete(b"delete").unwrap();
        let records = SQLiteRecordStore::for_key_value(&connection, &control).unwrap();
        records
            .allocate_identifiers(b"incremental-ids", IdentifierRequest::Observe(93), &control)
            .unwrap();
        let notification = KeyValueNotificationRecords.key(&control).unwrap();
        let wanted = expected(&records, &notification);
        let image = owner.capture_backup_image(&control).unwrap();
        store.put(b"change", b"after the cut").unwrap();
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
            report.source.copied_payload_bytes + 32 + 8 * wanted.len() as u64,
            report.inventory.decoded_bytes
        );
        let child = operation.publish(&control).unwrap();
        drop((operation, records, store, owner, connection));
        std::fs::remove_dir_all(source_path).unwrap();

        let (repository, child) = if standalone {
            copy_repository(repository, child, directory.path(), &control)
        } else {
            (repository, child)
        };
        let lease = repository.open_backup(child.backup(), &control).unwrap();
        let request = DatabaseRestore::new(child.cut().source).unwrap();
        let target = directory.path().join("target");
        empty_file(&target);
        let mut import =
            SQLiteKeyValueBackupImport::new(&target, &lease, request, options(mode), &control)
                .unwrap();
        complete(&mut import);
        let connection = open(&target, mode).unwrap();
        let records = SQLiteRecordStore::for_key_value(&connection, &control).unwrap();
        assert_eq!(expected(&records, &notification), wanted);
        drop(notification);
        assert_eq!(records.database_id(), request.target());
        assert_eq!(
            records
                .identifier_watermark(b"incremental-ids", &control)
                .unwrap(),
            Some(93)
        );
        let owner = SQLiteKeyValueStorage::from_connection(connection).unwrap();
        verify_diskann_restored(&owner.backend(), generation).unwrap();
        verify_diskann_restored_writes(&owner.backend(), generation).unwrap();
        owner.store().put(b"after-restore", b"writable").unwrap();
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
