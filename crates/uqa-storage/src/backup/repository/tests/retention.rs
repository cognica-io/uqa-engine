//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    image_fixture::{capture, objects, read, Image, Unit},
    *,
};
use std::sync::Arc;

mod failures;
mod generations;
mod processes;

fn repository(path: impl AsRef<std::path::Path>, control: &StorageReadControl) -> LocalRepository {
    LocalRepository::create(path, protected(), limits(), control).unwrap()
}

fn collect_all(
    repository: &LocalRepository,
    control: &StorageReadControl,
) -> BackupCollectionReport {
    for _ in 0..100 {
        let report = repository.collect_garbage(3, 16 << 20, control).unwrap();
        assert!(report.removed_objects + report.removed_staging_files <= 3);
        if report.remaining_files == 0 {
            return report;
        }
    }
    panic!("bounded collection did not finish")
}

#[test]
fn root_listing_is_ordered_bounded_metadata_and_excludes_prepared_operations() {
    let temporary = tempfile::tempdir().unwrap();
    let control = control();
    let repository = repository(temporary.path().join("repository"), &control);
    let mut expected = Vec::new();
    for key in 0..160 {
        expected.push(
            capture(
                &repository,
                None,
                vec![Unit::record(key, b"data")],
                &control,
                1 << 20,
            )
            .0,
        );
    }
    let source = Image::new(vec![Unit::record(999, b"private")]);
    let mut prepared = repository
        .begin_backup(source.cut(), None, 1 << 20, &control)
        .unwrap();
    prepared.prepare_image(source, 1 << 20, &control).unwrap();
    expected.sort_by_key(|root| root.backup());
    let listing_control = StorageReadControl::with_limit(64 << 10);
    assert!(repository.list_backups(0, &listing_control).is_err());
    assert_eq!(listing_control.memory().used(), 0);
    let mut listing = repository.list_backups(8 << 20, &listing_control).unwrap();
    repository
        .remove_backup(expected[0].backup(), &control)
        .unwrap();
    let mut actual = Vec::new();
    while let Some(root) = listing.next_backup().unwrap() {
        actual.push(root);
    }
    assert_eq!(actual, expected);
    assert_eq!(listing_control.memory().used(), 0);
    assert_eq!(listing.next_backup().unwrap(), None);
    let mut listing = repository.list_backups(8 << 20, &listing_control).unwrap();
    listing_control.cancellation().cancel();
    assert!(matches!(
        listing.next_backup(),
        Err(BackupError::Cancelled(_))
    ));
    assert_eq!(listing_control.memory().used(), 0);
    assert!(matches!(listing.next_backup(), Err(BackupError::Finished)));
}

#[test]
fn root_retirement_is_exclusive_durable_and_cannot_resurrect_prepared_publication() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("repository");
    let control = control();
    let repository = repository(&path, &control);
    let mut operation = repository
        .begin_backup(cut(), None, 1 << 20, &control)
        .unwrap();
    assert!(matches!(
        repository.remove_backup(operation.id(), &control),
        Err(BackupError::Busy)
    ));
    let root = prepare(&repository, &mut operation, &control);
    operation.publish(&control).unwrap();
    let first = repository.open_backup(root.backup(), &control).unwrap();
    let reopened = LocalRepository::open(&path, protected(), limits(), &control).unwrap();
    let second = reopened.open_backup(root.backup(), &control).unwrap();
    assert!(matches!(
        repository.remove_backup(root.backup(), &control),
        Err(BackupError::Busy)
    ));
    drop(first);
    assert!(matches!(
        repository.remove_backup(root.backup(), &control),
        Err(BackupError::Busy)
    ));
    second.verify(1 << 20, &control).unwrap();
    drop(second);
    repository.remove_backup(root.backup(), &control).unwrap();
    repository.remove_backup(root.backup(), &control).unwrap();
    // The old receipt reports its historical completion without reinstalling it.
    assert_eq!(operation.publish(&control).unwrap(), root);
    assert!(reopened.open_backup(root.backup(), &control).is_err());
    assert!(matches!(
        reopened.resume_publication(root.backup(), 1 << 20, &control),
        Err(BackupError::Finished)
    ));
    let mut prepared = repository
        .begin_backup(cut(), None, 1 << 20, &control)
        .unwrap();
    prepare(&repository, &mut prepared, &control);
    let id = prepared.id();
    drop(prepared);
    reopened.remove_backup(id, &control).unwrap();
    assert!(matches!(
        repository.resume_publication(id, 1 << 20, &control),
        Err(BackupError::Finished)
    ));
    collect_all(&repository, &control);
    assert_eq!(objects(&repository), (0, 0));
}

#[test]
fn collection_preserves_branched_children_and_counts_partial_packs_once() {
    for protection in [Protection::Plaintext, protected()] {
        let temporary = tempfile::tempdir().unwrap();
        let control = control();
        let repository = LocalRepository::create(
            temporary.path().join("repository"),
            protection,
            limits(),
            &control,
        )
        .unwrap();
        let units = vec![
            Unit::record(0, b"first"),
            Unit::record(1, b"second"),
            Unit::record(2, b"third"),
        ];
        let (parent, _) = capture(&repository, None, units.clone(), &control, 1 << 20);
        let spans = read(&repository, parent, &control);
        assert_eq!(spans[0].1.object(), spans[2].1.object());
        let (left, _) = capture(
            &repository,
            Some(parent.backup()),
            units[..1].to_vec(),
            &control,
            1 << 20,
        );
        let (right, _) = capture(
            &repository,
            Some(parent.backup()),
            units[2..].to_vec(),
            &control,
            1 << 20,
        );
        repository.remove_backup(parent.backup(), &control).unwrap();
        let report = collect_all(&repository, &control);
        assert_eq!(report.completed_backups, 2);
        assert_eq!(report.retained_object_bytes, objects(&repository).1);
        assert!(report.referenced_frame_bytes < report.retained_object_bytes);
        assert_eq!(read(&repository, left, &control)[0].0, units[0]);
        assert_eq!(read(&repository, right, &control)[0].0, units[2]);
        repository.remove_backup(left.backup(), &control).unwrap();
        collect_all(&repository, &control);
        assert_eq!(read(&repository, right, &control)[0].0, units[2]);
        let name = directory::name(spans[2].1.object().as_bytes());
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_eq!(
                directory::open_file(&repository.objects, directory::text(&name), false)
                    .unwrap()
                    .metadata()
                    .unwrap()
                    .nlink(),
                1
            );
        }
        repository.remove_backup(right.backup(), &control).unwrap();
        collect_all(&repository, &control);
        assert_eq!(objects(&repository), (0, 0));
    }
}

#[test]
fn prepared_roots_and_active_capture_objects_survive_collection_without_parent_manifests() {
    let temporary = tempfile::tempdir().unwrap();
    let control = control();
    let repository = repository(temporary.path().join("repository"), &control);
    let units = vec![Unit::record(0, b"unchanged")];
    let (parent, _) = capture(&repository, None, units.clone(), &control, 1 << 20);
    let source = Image::new(units.clone());
    let mut operation = repository
        .begin_backup(source.cut(), Some(parent.backup()), 1 << 20, &control)
        .unwrap();
    operation.prepare_image(source, 1 << 20, &control).unwrap();
    let child = operation.id();
    assert!(matches!(
        repository.remove_backup(parent.backup(), &control),
        Err(BackupError::Busy)
    ));
    drop(operation);
    repository.remove_backup(parent.backup(), &control).unwrap();
    let report = collect_all(&repository, &control);
    assert_eq!(report.prepared_backups, 1);
    assert_eq!(report.completed_backups, 0);
    let mut resumed = repository
        .resume_publication(child, 1 << 20, &control)
        .unwrap();
    let report = repository.collect_garbage(10, 1 << 20, &control).unwrap();
    assert_eq!(report.active_operations, 1);
    let child = resumed.publish(&control).unwrap();
    assert_eq!(read(&repository, child, &control)[0].0, units[0]);
    let mut active = repository
        .begin_backup(cut(), None, 1 << 20, &control)
        .unwrap();
    let span = active
        .write_object(FrameKind::Unit, &mut Cursor::new(b"unpublished"), &control)
        .unwrap();
    let before = objects(&repository);
    let report = repository.collect_garbage(100, 1 << 20, &control).unwrap();
    assert_eq!(report.active_operations, 1);
    assert_eq!(objects(&repository), before);
    let mut unfinished = active.begin_pack(&control).unwrap();
    unfinished
        .append(FrameKind::Unit, &mut Cursor::new(b"private"), &control)
        .unwrap();
    repository.collect_garbage(100, 1 << 20, &control).unwrap();
    drop(unfinished);
    drop(active);
    collect_all(&repository, &control);
    let name = directory::name(span.object().as_bytes());
    assert!(directory::open_file(&repository.objects, directory::text(&name), false).is_err());
    assert_eq!(read(&repository, child, &control)[0].0, units[0]);
}

#[test]
fn collection_revalidates_new_publication_removal_and_competing_collection() {
    for mutation in 0..3 {
        let temporary = tempfile::tempdir().unwrap();
        let control = control();
        let repository = Arc::new(repository(temporary.path().join("repository"), &control));
        let (root, _) = capture(
            &repository,
            None,
            vec![Unit::record(0, b"retained")],
            &control,
            1 << 20,
        );
        let changed = repository.clone();
        let token = control.clone();
        super::super::retention::ON_DISCOVERY.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(move || match mutation {
                0 => {
                    capture(
                        &changed,
                        None,
                        vec![Unit::record(1, b"published")],
                        &token,
                        1 << 20,
                    );
                }
                1 => changed.remove_backup(root.backup(), &token).unwrap(),
                2 => {
                    changed.collect_garbage(1, 1 << 20, &token).unwrap();
                }
                _ => unreachable!(),
            }));
        });
        assert!(matches!(
            repository.collect_garbage(10, 1 << 20, &control),
            Err(BackupError::Busy)
        ));
        collect_all(&repository, &control);
        if mutation != 1 {
            read(&repository, root, &control);
        }
    }
}

#[test]
fn preparation_cannot_publish_references_collected_after_full_verification() {
    let temporary = tempfile::tempdir().unwrap();
    let control = control();
    let repository = Arc::new(repository(temporary.path().join("repository"), &control));
    let (old, _) = capture(
        &repository,
        None,
        vec![Unit::record(0, b"retired")],
        &control,
        1 << 20,
    );
    repository.remove_backup(old.backup(), &control).unwrap();
    let mut operation = repository
        .begin_backup(old.cut(), None, 1 << 20, &control)
        .unwrap();
    let changed = repository.clone();
    let token = control.clone();
    super::super::operation::ON_PREPARED_VERIFIED.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move || {
            collect_all(&changed, &token);
            assert_eq!(objects(&changed), (0, 0));
        }));
    });
    assert!(matches!(
        operation.prepare(old.inventory(), old.summary(), &control),
        Err(BackupError::Busy)
    ));
    assert!(matches!(
        operation.publish(&control),
        Err(BackupError::Finished)
    ));
    let id = operation.id();
    drop(operation);
    assert!(repository
        .resume_publication(id, 1 << 20, &control)
        .is_err());
    assert!(repository.open_backup(id, &control).is_err());
}

#[test]
fn root_listing_rejects_a_concurrent_generation_without_partial_metadata() {
    let temporary = tempfile::tempdir().unwrap();
    let control = control();
    let repository = Arc::new(repository(temporary.path().join("repository"), &control));
    let (root, _) = capture(
        &repository,
        None,
        vec![Unit::record(0, b"retained")],
        &control,
        1 << 20,
    );
    let changed = repository.clone();
    let token = control.clone();
    super::super::retention::ON_LIST_DISCOVERY.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move || {
            changed.remove_backup(root.backup(), &token).unwrap();
        }));
    });
    assert!(matches!(
        repository.list_backups(1 << 20, &control),
        Err(BackupError::Busy)
    ));
    assert_eq!(control.memory().used(), 0);
    assert!(repository
        .list_backups(1 << 20, &control)
        .unwrap()
        .next_backup()
        .unwrap()
        .is_none());
}
