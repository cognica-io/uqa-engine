//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::io::{Seek, SeekFrom, Write};

#[test]
fn interrupted_root_removal_keeps_discoverable_content_and_resolves_after_reopen() {
    for fault in [Fault::RemovalMarker, Fault::RemovalRoot, Fault::RemovalSync] {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("repository");
        let control = control();
        let repository = repository(&path, &control);
        let (root, _) = capture(
            &repository,
            None,
            vec![Unit::record(0, b"retained")],
            &control,
            1 << 20,
        );
        *repository.fault.lock() = Some(fault);
        assert!(repository.remove_backup(root.backup(), &control).is_err());
        let reopened = LocalRepository::open(&path, protected(), limits(), &control).unwrap();
        collect_all(&reopened, &control);
        if fault != Fault::RemovalSync {
            read(&reopened, root, &control);
        }
        reopened.remove_backup(root.backup(), &control).unwrap();
        assert!(matches!(
            reopened.resume_publication(root.backup(), 1 << 20, &control),
            Err(BackupError::Finished)
        ));
        collect_all(&reopened, &control);
        assert_eq!(objects(&reopened), (0, 0));
    }
}

#[test]
fn collection_limits_and_cancellation_release_workspace_before_any_deletion() {
    let temporary = tempfile::tempdir().unwrap();
    let control = control();
    let repository = repository(temporary.path().join("repository"), &control);
    let (root, _) = capture(
        &repository,
        None,
        (0..1100).map(|key| Unit::record(key, b"data")).collect(),
        &control,
        8 << 20,
    );
    let before = objects(&repository);
    assert!(repository.collect_garbage(0, 8 << 20, &control).is_err());
    let tiny = StorageReadControl::with_limit(1);
    assert!(repository.collect_garbage(10, 8 << 20, &tiny).is_err());
    assert_eq!(tiny.memory().used(), 0);
    let bounded = StorageReadControl::with_limit(128 << 10);
    assert!(matches!(
        repository.collect_garbage(10, 0, &bounded),
        Err(BackupError::Limit { .. })
    ));
    assert_eq!(objects(&repository), before);
    assert_eq!(bounded.memory().used(), 0);
    bounded.cancellation().cancel();
    assert!(matches!(
        repository.collect_garbage(10, 8 << 20, &bounded),
        Err(BackupError::Cancelled(_))
    ));
    let bounded = StorageReadControl::with_limit(128 << 10);
    let report = repository.collect_garbage(1, 16 << 20, &bounded).unwrap();
    assert!(report.workspace_peak_bytes > 0);
    assert!(report.removed_staging_files + report.removed_objects <= 1);
    assert_eq!(bounded.memory().used(), 0);
    assert!(matches!(
        repository.collect_garbage(1, report.workspace_peak_bytes - 1, &bounded),
        Err(BackupError::Limit { .. })
    ));
    assert_eq!(bounded.memory().used(), 0);
    assert_eq!(read(&repository, root, &control).len(), 1100);
}

#[test]
fn corrupt_inventory_or_missing_referenced_objects_prevent_all_collection_mutations() {
    for damage in 0..3 {
        let temporary = tempfile::tempdir().unwrap();
        let control = control();
        let repository = repository(temporary.path().join("repository"), &control);
        let (root, _) = capture(
            &repository,
            None,
            vec![Unit::record(0, b"retained")],
            &control,
            1 << 20,
        );
        let span = if damage == 0 {
            root.inventory()
        } else {
            read(&repository, root, &control)[0].1
        };
        let name = directory::name(span.object().as_bytes());
        if damage == 2 {
            repository
                .objects
                .remove_file(directory::text(&name))
                .unwrap();
        } else {
            let mut file =
                directory::open_file(&repository.objects, directory::text(&name), true).unwrap();
            if damage == 1 {
                file.set_len(1).unwrap();
            } else {
                file.seek(SeekFrom::Start(span.offset() + 100)).unwrap();
                file.write_all(b"broken").unwrap();
            }
            file.sync_all().unwrap();
        }
        let before = objects(&repository);
        let operation_name = directory::name(root.backup().as_bytes());
        let operation = directory::child(
            &repository.operations,
            directory::text(&operation_name).as_ref(),
        )
        .unwrap();
        let before_staging = operation.entries().unwrap().count();
        assert!(repository.collect_garbage(100, 1 << 20, &control).is_err());
        assert_eq!(objects(&repository), before);
        assert_eq!(operation.entries().unwrap().count(), before_staging);
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn collection_io_failures_are_retryable_and_never_remove_live_objects() {
    for fault in [Fault::CollectionUnlink, Fault::CollectionSync] {
        let temporary = tempfile::tempdir().unwrap();
        let control = control();
        let repository = repository(temporary.path().join("repository"), &control);
        let (live, _) = capture(
            &repository,
            None,
            vec![Unit::record(0, b"live")],
            &control,
            1 << 20,
        );
        let (dead, _) = capture(
            &repository,
            None,
            vec![Unit::record(1, b"dead")],
            &control,
            1 << 20,
        );
        repository.remove_backup(dead.backup(), &control).unwrap();
        *repository.fault.lock() = Some(fault);
        assert!(repository.collect_garbage(10, 1 << 20, &control).is_err());
        read(&repository, live, &control);
        let report = collect_all(&repository, &control);
        assert_eq!(report.completed_backups, 1);
        assert_eq!(
            report.unreferenced_object_bytes,
            report.removed_object_bytes
        );
        assert_eq!(report.retained_object_bytes, objects(&repository).1);
        assert_eq!(control.memory().used(), 0);
    }
}

#[cfg(unix)]
#[test]
fn collection_rejects_substituted_paths_without_touching_external_files() {
    for area in ["roots", "objects", "operations"] {
        let temporary = tempfile::tempdir().unwrap();
        let control = control();
        let path = temporary.path().join("repository");
        let repository = repository(&path, &control);
        let (root, _) = capture(
            &repository,
            None,
            vec![Unit::record(0, b"live")],
            &control,
            1 << 20,
        );
        let external = temporary.path().join("external");
        std::fs::write(&external, b"untouched").unwrap();
        let name = directory::name(ObjectId::new().unwrap().as_bytes());
        std::os::unix::fs::symlink(&external, path.join(area).join(directory::text(&name)))
            .unwrap();
        let before = objects(&repository);
        assert!(repository.collect_garbage(100, 1 << 20, &control).is_err());
        assert_eq!(std::fs::read(&external).unwrap(), b"untouched");
        assert_eq!(objects(&repository), before);
        std::fs::remove_file(path.join(area).join(directory::text(&name))).unwrap();
        read(&repository, root, &control);
    }
}

#[test]
fn interrupted_empty_operation_creation_does_not_block_collection() {
    let temporary = tempfile::tempdir().unwrap();
    let control = control();
    let repository = repository(temporary.path().join("repository"), &control);
    let name = directory::name(BackupId::new().unwrap().as_bytes());
    directory::create_directory(&repository.operations, directory::text(&name)).unwrap();
    assert_eq!(
        repository
            .collect_garbage(1, 1 << 20, &control)
            .unwrap()
            .remaining_files,
        0
    );
    // Missing ownership on a nonempty operation is an error, not permission to
    // reclaim its files or adopt an unrelated file as a lease.
    let operation =
        directory::child(&repository.operations, directory::text(&name).as_ref()).unwrap();
    directory::create_file(&operation, "intent").unwrap();
    assert!(repository.collect_garbage(1, 1 << 20, &control).is_err());
}
