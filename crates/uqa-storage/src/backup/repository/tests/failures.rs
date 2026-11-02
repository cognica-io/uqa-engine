//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn process_exit_releases_lease_and_preserves_prepared_image() {
    const REPOSITORY: &str = "UQA_BACKUP_LEASE_CHILD_REPOSITORY";
    if let Some(path) = std::env::var_os(REPOSITORY) {
        let path = std::path::PathBuf::from(path);
        let control = control();
        let repository = LocalRepository::create(&path, protected(), limits(), &control).unwrap();
        let mut operation = repository
            .begin_backup(cut(), None, 1024 * 1024, &control)
            .unwrap();
        prepare(&repository, &mut operation, &control);
        std::fs::write(
            path.parent().unwrap().join("operation"),
            operation.id().as_bytes(),
        )
        .unwrap();
        // Exit without Rust destructors while the prepared operation owns its OS lease.
        std::process::exit(0);
    }
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("backup");
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "backup::repository::tests::failures::process_exit_releases_lease_and_preserves_prepared_image"])
        .env(REPOSITORY, &path).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let id = BackupId::from_bytes(
        std::fs::read(directory.path().join("operation"))
            .unwrap()
            .try_into()
            .unwrap(),
    )
    .unwrap();
    let control = control();
    let repository = LocalRepository::open(&path, protected(), limits(), &control).unwrap();
    let mut resumed = repository
        .resume_publication(id, 1024 * 1024, &control)
        .unwrap();
    let root = resumed.publish(&control).unwrap();
    assert_eq!(root.backup(), id);
    repository
        .open_backup(id, &control)
        .unwrap()
        .verify(1024 * 1024, &control)
        .unwrap();
}

#[test]
fn a_complete_prepared_file_is_synchronized_again_during_recovery() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("backup");
    let control = control();
    let repository = LocalRepository::create(&path, protected(), limits(), &control).unwrap();
    let mut operation = repository
        .begin_backup(cut(), None, 1024 * 1024, &control)
        .unwrap();
    let (bytes, summary) =
        InventoryWriter::new(Vec::new(), operation.cut().format, limits(), &control)
            .unwrap()
            .finish()
            .unwrap();
    let inventory = operation
        .write_object(FrameKind::Inventory, &mut Cursor::new(bytes), &control)
        .unwrap();
    *repository.fault.lock() = Some(Fault::PreparedSync);
    assert!(operation.prepare(inventory, summary, &control).is_err());
    assert!(matches!(
        operation.publish(&control),
        Err(BackupError::Finished)
    ));
    let id = operation.id();
    drop(operation);
    drop(repository);
    let repository = LocalRepository::open(&path, protected(), limits(), &control).unwrap();
    let mut resumed = repository
        .resume_publication(id, 1024 * 1024, &control)
        .unwrap();
    let root = resumed.publish(&control).unwrap();
    assert_eq!(root.summary(), summary);
    repository
        .open_backup(id, &control)
        .unwrap()
        .verify(1024 * 1024, &control)
        .unwrap();
}

#[test]
fn object_sync_and_install_failures_cannot_publish_a_capture() {
    for fault in [Fault::ObjectSync, Fault::ObjectInstall] {
        let directory = tempfile::tempdir().unwrap();
        let control = control();
        let repository = LocalRepository::create(
            directory.path().join("backup"),
            protected(),
            limits(),
            &control,
        )
        .unwrap();
        let mut operation = repository
            .begin_backup(cut(), None, 1024 * 1024, &control)
            .unwrap();
        *repository.fault.lock() = Some(fault);
        assert!(operation
            .write_object(FrameKind::Unit, &mut Cursor::new(b"partial"), &control)
            .is_err());
        assert!(matches!(
            operation.publish(&control),
            Err(BackupError::Finished)
        ));
        assert_eq!(repository.roots.entries().unwrap().count(), 0);
        assert_eq!(repository.objects.entries().unwrap().count(), 0);
    }
}

#[test]
fn publication_install_failure_keeps_exact_prepared_image_for_retry() {
    let directory = tempfile::tempdir().unwrap();
    let control = control();
    let repository = LocalRepository::create(
        directory.path().join("backup"),
        protected(),
        limits(),
        &control,
    )
    .unwrap();
    let mut operation = repository
        .begin_backup(cut(), None, 1024 * 1024, &control)
        .unwrap();
    let root = prepare(&repository, &mut operation, &control);
    *repository.fault.lock() = Some(Fault::RootInstall);
    assert!(operation.publish(&control).is_err());
    assert!(repository.open_backup(root.backup(), &control).is_err());
    assert_eq!(operation.publish(&control).unwrap(), root);
}

#[test]
fn root_sync_failure_resolves_completion_on_cancel_and_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("backup");
    let control = control();
    let repository = LocalRepository::create(&path, protected(), limits(), &control).unwrap();
    let mut operation = repository
        .begin_backup(cut(), None, 1024 * 1024, &control)
        .unwrap();
    let root = prepare(&repository, &mut operation, &control);
    *repository.fault.lock() = Some(Fault::RootSync);
    assert!(operation.publish(&control).is_err());
    assert_eq!(operation.cancel().unwrap(), Some(root));
    drop(operation);
    drop(repository);
    let repository = LocalRepository::open(&path, protected(), limits(), &control).unwrap();
    repository
        .open_backup(root.backup(), &control)
        .unwrap()
        .verify(1024 * 1024, &control)
        .unwrap();
}

#[test]
fn failed_cancel_sync_remains_cancelling_and_is_retryable() {
    let directory = tempfile::tempdir().unwrap();
    let control = control();
    let repository = LocalRepository::create(
        directory.path().join("backup"),
        protected(),
        limits(),
        &control,
    )
    .unwrap();
    let mut operation = repository
        .begin_backup(cut(), None, 1024 * 1024, &control)
        .unwrap();
    prepare(&repository, &mut operation, &control);
    *repository.fault.lock() = Some(Fault::CancelSync);
    assert!(operation.cancel().is_err());
    assert!(matches!(
        operation.publish(&control),
        Err(BackupError::Finished)
    ));
    assert_eq!(operation.cancel().unwrap(), None);
    assert!(matches!(
        repository.resume_publication(operation.id(), 1024 * 1024, &control),
        Err(BackupError::Finished)
    ));
}
