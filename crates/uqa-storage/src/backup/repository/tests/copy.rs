//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::image_fixture::{capture, objects, read, Unit};
use super::*;
use std::io::{Seek, SeekFrom, Write};

mod failures;

fn protection(index: u8) -> Protection {
    if index == 0 {
        Protection::Plaintext
    } else {
        Protection::Encrypted {
            domain: ProtectionId::from_bytes([index; 16]).unwrap(),
            key: BackupKey::new([index + 40; 32]),
        }
    }
}

fn repository(path: impl AsRef<Path>, mode: u8, control: &StorageReadControl) -> LocalRepository {
    LocalRepository::create(path, protection(mode), limits(), control).unwrap()
}

#[test]
fn standalone_copy_reprotects_complete_incremental_images_after_source_loss() {
    for source_mode in 0..3 {
        for destination_mode in 0..3 {
            let temporary = tempfile::tempdir().unwrap();
            let source_path = temporary.path().join("source");
            let destination_path = temporary.path().join("destination");
            let control = control();
            let source = repository(&source_path, source_mode, &control);
            let mut original = vec![
                Unit::record(0, b""),
                Unit::record(1, b"retained secret bytes"),
                Unit::record(2, b"deleted bytes"),
                Unit::record(3, &[7; 20000]),
                Unit::record(4, b""),
            ];
            original[4].kind = UnitKind::Tombstone;
            let (parent, _) = capture(&source, None, original.clone(), &control, 4 << 20);
            original.remove(2);
            original.push(Unit::record(5, b"new child contents"));
            let (child, incremental) = capture(
                &source,
                Some(parent.backup()),
                original.clone(),
                &control,
                4 << 20,
            );
            assert_eq!(incremental.image.reused_units, 4);
            let source_units = read(&source, child, &control);
            let lease = source.open_backup(child.backup(), &control).unwrap();
            let destination = repository(&destination_path, destination_mode, &control);
            let mut copy = destination
                .begin_backup(child.cut(), None, 4 << 20, &control)
                .unwrap();
            let report = copy.prepare_copy(&lease, 4 << 20, &control).unwrap();
            assert_eq!(report.inventory, child.summary());
            assert_eq!(report.copied_payload_bytes, child.summary().decoded_bytes);
            assert!(report.verified_source_object_bytes > report.copied_payload_bytes);
            assert_eq!(report.written_object_bytes, objects(&destination).1);
            assert!(report.workspace_peak_bytes > 0 && report.workspace_peak_bytes <= 4 << 20);
            assert!(destination.open_backup(copy.id(), &control).is_err());
            assert!(matches!(
                copy.prepare_copy(&lease, 4 << 20, &control),
                Err(BackupError::Finished)
            ));
            let copied = copy.publish(&control).unwrap();
            assert_eq!(copied.cut(), child.cut());
            assert_eq!(copied.summary(), child.summary());
            assert_eq!(copied.parent(), None);
            assert_ne!(copied.repository(), child.repository());
            assert_ne!(copied.backup(), child.backup());
            drop(copy);
            drop(lease);
            drop(source);
            std::fs::remove_dir_all(&source_path).unwrap();
            drop(destination);
            if destination_mode != 0 {
                assert!(LocalRepository::open(
                    &destination_path,
                    protection(0),
                    limits(),
                    &control
                )
                .is_err());
                let wrong = Protection::Encrypted {
                    domain: ProtectionId::from_bytes([destination_mode; 16]).unwrap(),
                    key: BackupKey::new([0; 32]),
                };
                assert!(matches!(
                    LocalRepository::open(&destination_path, wrong, limits(), &control),
                    Err(BackupError::Integrity)
                ));
                for entry in std::fs::read_dir(destination_path.join("objects")).unwrap() {
                    let bytes = std::fs::read(entry.unwrap().path()).unwrap();
                    assert!(!bytes
                        .windows(b"retained secret bytes".len())
                        .any(|window| window == b"retained secret bytes"));
                }
            }
            let reopened = LocalRepository::open(
                &destination_path,
                protection(destination_mode),
                limits(),
                &control,
            )
            .unwrap();
            let restored = read(&reopened, copied, &control);
            assert_eq!(
                restored.iter().map(|(unit, _)| unit).collect::<Vec<_>>(),
                original.iter().collect::<Vec<_>>()
            );
            for (_, span) in &restored {
                assert!(source_units
                    .iter()
                    .all(|(_, source_span)| source_span.object() != span.object()));
            }
            assert_eq!(control.memory().used(), 0);
        }
    }
}

#[test]
fn standalone_copy_prepared_and_uncertain_publications_need_only_the_destination_key() {
    for uncertain in [false, true] {
        let temporary = tempfile::tempdir().unwrap();
        let control = control();
        let source_path = temporary.path().join("source");
        let destination_path = temporary.path().join("destination");
        let source = repository(&source_path, 1, &control);
        let (original, _) = capture(
            &source,
            None,
            vec![Unit::record(0, b"rotated key")],
            &control,
            1 << 20,
        );
        let lease = source.open_backup(original.backup(), &control).unwrap();
        let destination = repository(&destination_path, 2, &control);
        let mut copy = destination
            .begin_backup(original.cut(), None, 1 << 20, &control)
            .unwrap();
        let id = copy.id();
        copy.prepare_copy(&lease, 1 << 20, &control).unwrap();
        drop(lease);
        drop(source);
        std::fs::remove_dir_all(&source_path).unwrap();
        if uncertain {
            *destination.fault.lock() = Some(Fault::RootSync);
            assert!(copy.publish(&control).is_err());
        }
        drop(copy);
        drop(destination);
        let reopened =
            LocalRepository::open(&destination_path, protection(2), limits(), &control).unwrap();
        let mut resumed = reopened.resume_publication(id, 1 << 20, &control).unwrap();
        let cancellation = CancellationToken::new();
        let cancelled = StorageReadControl::new(control.memory(), &cancellation);
        cancellation.cancel();
        let root = if uncertain {
            resumed.publish(&cancelled).unwrap()
        } else {
            resumed.publish(&control).unwrap()
        };
        assert_eq!(root.summary(), original.summary());
        assert_eq!(resumed.publish(&cancelled).unwrap(), root);
        assert_eq!(resumed.cancel().unwrap(), Some(root));
        assert_eq!(read(&reopened, root, &control)[0].0.data, b"rotated key");
    }
}

#[test]
fn standalone_copy_bounds_large_units_and_shared_verification_workspace() {
    let temporary = tempfile::tempdir().unwrap();
    let source_control = control();
    let source = repository(temporary.path().join("source"), 1, &source_control);
    let mut units = (0..1030)
        .map(|id| Unit::record(id, &[id as u8; 32]))
        .collect::<Vec<_>>();
    units[1].data = vec![7; (2 << 20) + 7];
    let (root, _) = capture(&source, None, units, &source_control, 8 << 20);
    let lease = source.open_backup(root.backup(), &source_control).unwrap();
    let control = StorageReadControl::with_limit(128 * 1024);
    let destination = repository(temporary.path().join("destination"), 2, &control);
    let mut copy = destination
        .begin_backup(root.cut(), None, 16 << 20, &control)
        .unwrap();
    let report = copy.prepare_copy(&lease, 8 << 20, &control).unwrap();
    assert_eq!(report.inventory, root.summary());
    assert!(report.workspace_peak_bytes > 64 * 1024);
    assert!(report.workspace_peak_bytes <= 8 << 20);
    assert_eq!(control.memory().used(), 0);
    assert!(control.memory().peak() <= 128 * 1024);
    let copied = copy.publish(&control).unwrap();
    drop(copy);
    destination
        .open_backup(copied.backup(), &control)
        .unwrap()
        .verify(8 << 20, &control)
        .unwrap();
    let mut limited = destination
        .begin_backup(root.cut(), None, 16 << 20, &control)
        .unwrap();
    assert!(matches!(
        limited.prepare_copy(&lease, report.workspace_peak_bytes - 1, &control),
        Err(BackupError::Limit { .. })
    ));
    assert!(matches!(
        limited.publish(&control),
        Err(BackupError::Finished)
    ));
    assert_eq!(control.memory().used(), 0);
    assert_eq!(limited.cancel().unwrap(), None);
}

#[test]
fn standalone_copy_preserves_an_empty_complete_image() {
    let temporary = tempfile::tempdir().unwrap();
    let control = control();
    let source = repository(temporary.path().join("source"), 0, &control);
    let (root, _) = capture(&source, None, Vec::new(), &control, 1 << 20);
    let destination = repository(temporary.path().join("destination"), 1, &control);
    let lease = source.open_backup(root.backup(), &control).unwrap();
    let mut copy = destination
        .begin_backup(root.cut(), None, 1 << 20, &control)
        .unwrap();
    let report = copy.prepare_copy(&lease, 1 << 20, &control).unwrap();
    assert_eq!(report.inventory, root.summary());
    assert_eq!(report.copied_payload_bytes, 0);
    let copied = copy.publish(&control).unwrap();
    assert!(read(&destination, copied, &control).is_empty());
}
