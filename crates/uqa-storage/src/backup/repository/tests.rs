//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::io::Cursor;

use crate::backup::*;
use crate::mvcc::DatabaseId;
use crate::read_control::{CancellationToken, StorageReadControl};

use super::*;

mod capture;
mod failures;
mod integrity;
mod resources;
mod streaming;

fn control() -> StorageReadControl {
    StorageReadControl::with_limit(512 * 1024)
}
fn limits() -> BackupLimits {
    BackupLimits::new(4096, 4096).unwrap()
}
fn protected() -> Protection {
    Protection::Encrypted {
        domain: ProtectionId::from_bytes([1; 16]).unwrap(),
        key: BackupKey::new([42; 32]),
    }
}
fn cut() -> BackupCut {
    BackupCut {
        source: DatabaseId::from_bytes([2; 16]),
        snapshot: SnapshotId::new().unwrap(),
        format: ImageFormat::new(ImageProvider::Memory, 1).unwrap(),
        sequence: None,
    }
}

fn prepare(
    repository: &LocalRepository,
    operation: &mut BackupOperation<'_>,
    control: &StorageReadControl,
) -> BackupRoot {
    let data = b"retained record contents";
    let span = operation
        .write_object(FrameKind::Unit, &mut Cursor::new(data), control)
        .unwrap();
    let entry = InventoryEntry::new(
        UnitAddress {
            namespace: NamespaceId::from_bytes([3; 16]).unwrap(),
            generation: 1,
            family: 1,
            key: b"record",
        },
        UnitKind::Record,
        data.len() as u64,
        ImageDigest::of_bytes(data),
        span,
        limits(),
        control,
    )
    .unwrap();
    let mut writer = InventoryWriter::new(Vec::new(), cut().format, limits(), control).unwrap();
    writer.append(&entry).unwrap();
    let (bytes, summary) = writer.finish().unwrap();
    let inventory = operation
        .write_object(FrameKind::Inventory, &mut Cursor::new(bytes), control)
        .unwrap();
    operation.prepare(inventory, summary, control).unwrap();
    repository
        .read_root(operation.id(), control)
        .expect_err("prepared root remains private");
    BackupRoot::new(
        repository.id(),
        operation.id(),
        None,
        operation.cut(),
        inventory,
        summary,
    )
    .unwrap()
}

#[test]
fn repository_authenticates_identity_and_never_adopts_existing_destinations() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("backup");
    let control = control();
    let repository = LocalRepository::create(&path, protected(), limits(), &control).unwrap();
    assert!(LocalRepository::create(&path, protected(), limits(), &control).is_err());
    let reopened = LocalRepository::open(&path, protected(), limits(), &control).unwrap();
    assert_eq!(repository.id(), reopened.id());
    assert!(LocalRepository::open(&path, Protection::Plaintext, limits(), &control).is_err());
    let wrong = Protection::Encrypted {
        domain: ProtectionId::from_bytes([1; 16]).unwrap(),
        key: BackupKey::new([43; 32]),
    };
    assert!(matches!(
        LocalRepository::open(&path, wrong, limits(), &control),
        Err(BackupError::Integrity)
    ));
}

#[test]
fn only_verified_published_roots_are_discoverable_and_hold_shared_leases() {
    for protection in [Protection::Plaintext, protected()] {
        let directory = tempfile::tempdir().unwrap();
        let control = control();
        let repository = LocalRepository::create(
            directory.path().join("backup"),
            protection,
            limits(),
            &control,
        )
        .unwrap();
        let mut operation = repository
            .begin_backup(cut(), None, 1024 * 1024, &control)
            .unwrap();
        assert!(repository.open_backup(operation.id(), &control).is_err());
        let expected = prepare(&repository, &mut operation, &control);
        assert_eq!(operation.publish(&control).unwrap(), expected);
        assert_eq!(operation.publish(&control).unwrap(), expected);
        let first = repository.open_backup(operation.id(), &control).unwrap();
        let second = repository.open_backup(operation.id(), &control).unwrap();
        first.verify(1024 * 1024, &control).unwrap();
        second.verify(1024 * 1024, &control).unwrap();
        assert!(matches!(
            repository.resume_publication(operation.id(), 1024 * 1024, &control),
            Err(BackupError::Busy)
        ));
    }
}

#[test]
fn durable_prepared_capture_resumes_without_the_source() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("backup");
    let control = control();
    let (id, expected) = {
        let repository = LocalRepository::create(&path, protected(), limits(), &control).unwrap();
        let mut operation = repository
            .begin_backup(cut(), None, 1024 * 1024, &control)
            .unwrap();
        let root = prepare(&repository, &mut operation, &control);
        (operation.id(), root)
    };
    let repository = LocalRepository::open(&path, protected(), limits(), &control).unwrap();
    let mut resumed = repository
        .resume_publication(id, 1024 * 1024, &control)
        .unwrap();
    assert_eq!(resumed.publish(&control).unwrap(), expected);
    drop(resumed);
    let mut resumed = repository
        .resume_publication(id, 1024 * 1024, &control)
        .unwrap();
    assert_eq!(resumed.publish(&control).unwrap(), expected);
}

#[test]
fn cancellation_is_durable_and_cannot_resurrect_a_prepared_capture() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("backup");
    let control = control();
    let repository = LocalRepository::create(&path, protected(), limits(), &control).unwrap();
    for prepared in [false, true] {
        let mut operation = repository
            .begin_backup(cut(), None, 1024 * 1024, &control)
            .unwrap();
        if prepared {
            prepare(&repository, &mut operation, &control);
        }
        let id = operation.id();
        assert_eq!(operation.cancel().unwrap(), None);
        assert_eq!(operation.cancel().unwrap(), None);
        assert!(matches!(
            operation.publish(&control),
            Err(BackupError::Finished)
        ));
        drop(operation);
        let reopened = LocalRepository::open(&path, protected(), limits(), &control).unwrap();
        assert!(matches!(
            reopened.resume_publication(id, 1024 * 1024, &control),
            Err(BackupError::Finished)
        ));
        assert!(reopened.open_backup(id, &control).is_err());
    }
}

#[test]
fn lost_publication_reply_resolves_before_late_cancellation() {
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
    let expected = prepare(&repository, &mut operation, &control);
    // A process can lose its reply after linking the root and before recording completion.
    let name = directory::name(operation.id().as_bytes());
    let captured =
        directory::child(&repository.operations, directory::text(&name).as_ref()).unwrap();
    captured
        .hard_link("prepared", &repository.roots, directory::text(&name))
        .unwrap();
    let cancellation = CancellationToken::new();
    let cancelled = StorageReadControl::new(control.memory(), &cancellation);
    cancellation.cancel();
    assert_eq!(operation.publish(&cancelled).unwrap(), expected);
    assert_eq!(operation.cancel().unwrap(), Some(expected));
    repository
        .open_backup(expected.backup(), &control)
        .unwrap()
        .verify(1024 * 1024, &control)
        .unwrap();
}

#[test]
fn failed_capture_cannot_continue_or_publish() {
    let directory = tempfile::tempdir().unwrap();
    let control = control();
    let repository = LocalRepository::create(
        directory.path().join("backup"),
        protected(),
        limits(),
        &control,
    )
    .unwrap();
    let mut operation = repository.begin_backup(cut(), None, 300, &control).unwrap();
    let error = operation
        .write_object(FrameKind::Unit, &mut Cursor::new([7; 1000]), &control)
        .unwrap_err();
    assert!(matches!(error, BackupError::Limit { .. }));
    assert!(matches!(
        operation.write_object(FrameKind::Unit, &mut Cursor::new(b"x"), &control),
        Err(BackupError::Finished)
    ));
    assert!(matches!(
        operation.publish(&control),
        Err(BackupError::Finished)
    ));
    let id = operation.id();
    drop(operation);
    assert!(repository
        .resume_publication(id, 1024 * 1024, &control)
        .is_err());
}
