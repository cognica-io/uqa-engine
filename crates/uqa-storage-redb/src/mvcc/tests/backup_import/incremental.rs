//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn redb_incremental_backup_restores_changed_membership_and_inherited_artifacts() {
    verify_incremental_restore(false);
}

#[test]
fn redb_standalone_copy_restores_incremental_images_without_the_old_repository() {
    verify_incremental_restore(true);
}

fn verify_incremental_restore(standalone: bool) {
    let directory = tempfile::tempdir().unwrap();
    let source_path = directory.path().join("source.redb");
    let target_path = directory.path().join("target.redb");
    let source = RedbStorage::open(&source_path).unwrap();
    let control = StorageReadControl::with_limit(1 << 20);
    let session = source.open_session().unwrap();
    let generation = verify_diskann_restore_source(&*session.backend).unwrap();
    drop(session);
    let store = source.store();
    store.put(b"keep", b"unchanged").unwrap();
    store.put(b"change", b"before").unwrap();
    store.put(b"delete", b"removed").unwrap();
    let (repository, parent) = capture(&source, &directory.path().join("backup"), &control);
    store.put(b"change", b"after!").unwrap();
    store.put(b"new", b"added").unwrap();
    store.delete(b"delete").unwrap();
    let records = source.record_store().unwrap();
    records
        .allocate_identifiers(b"incremental-ids", IdentifierRequest::Observe(93), &control)
        .unwrap();
    let notification = KeyValueNotificationRecords.key(&control).unwrap();
    let expected =
        super::super::backup_image::expected_records(&source, b"\xffunused", &notification);
    let image = source.capture_backup_image(&control).unwrap();
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
        report.source.copied_payload_bytes + 32 + 8 * expected.len() as u64,
        report.inventory.decoded_bytes
    );
    let child = operation.publish(&control).unwrap();
    drop((operation, records, store, source));
    std::fs::remove_file(source_path).unwrap();

    let (repository, child) = if standalone {
        copy_repository(repository, child, directory.path(), &control)
    } else {
        (repository, child)
    };
    let lease = repository.open_backup(child.backup(), &control).unwrap();
    let request = DatabaseRestore::new(child.cut().source).unwrap();
    let mut import = RedbBackupImport::new(
        file(&target_path),
        &lease,
        request,
        1 << 20,
        2 << 20,
        &control,
    )
    .unwrap();
    complete_import(&mut import);
    let restored = RedbStorage::open(&target_path).unwrap();
    assert_eq!(
        super::super::backup_image::expected_records(&restored, b"\xffunused", &notification),
        expected,
    );
    drop(notification);
    let records = restored.record_store().unwrap();
    assert_eq!(records.database_id(), request.target());
    assert_eq!(
        records
            .identifier_watermark(b"incremental-ids", &control)
            .unwrap(),
        Some(93)
    );
    let session = restored.open_session().unwrap();
    verify_diskann_restored(&*session.backend, generation).unwrap();
    verify_diskann_restored_writes(&*session.backend, generation).unwrap();
    restored.store().put(b"after-restore", b"writable").unwrap();
    assert_eq!(control.memory().used(), 0);
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
