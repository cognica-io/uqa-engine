//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn publish(repository: &LocalRepository, control: &StorageReadControl) -> BackupRoot {
    let mut operation = repository
        .begin_backup(cut(), None, 1 << 20, control)
        .unwrap();
    prepare(repository, &mut operation, control);
    operation.publish(control).unwrap()
}

#[test]
fn owned_backup_reader_releases_the_root_only_after_complete_verification() {
    for protection in [Protection::Plaintext, protected()] {
        let directory = tempfile::tempdir().unwrap();
        let setup = control();
        let repository = LocalRepository::create(
            directory.path().join("backup"),
            protection,
            limits(),
            &setup,
        )
        .unwrap();
        let root = publish(&repository, &setup);
        let control = StorageReadControl::with_limit(128 << 10);
        let mut reader = {
            let lease = repository.open_backup(root.backup(), &control).unwrap();
            lease.into_image_reader(1 << 20, &control).unwrap()
        };
        assert!(matches!(
            repository.remove_backup(root.backup(), &setup),
            Err(BackupError::Busy)
        ));
        assert_eq!(reader.advance(7).unwrap(), BackupReadStep::Unit);
        let mut actual = Vec::new();
        let mut buffer = [0; 7];
        loop {
            let read = reader.read_current(&mut buffer).unwrap();
            if read == 0 {
                break;
            }
            actual.extend_from_slice(&buffer[..read]);
        }
        assert_eq!(actual, b"retained record contents");
        loop {
            assert!(reader.verified().is_none());
            assert!(matches!(
                repository.remove_backup(root.backup(), &setup),
                Err(BackupError::Busy)
            ));
            let before = reader.verified_object_bytes();
            let step = reader.advance(7).unwrap();
            assert!(reader.verified_object_bytes() - before <= 7);
            if step == BackupReadStep::Complete {
                break;
            }
            assert_eq!(step, BackupReadStep::Pending);
        }
        assert_eq!(reader.verified().unwrap().root(), root);
        assert_eq!(control.memory().used(), 0);
        repository.remove_backup(root.backup(), &setup).unwrap();
        control.cancellation().cancel();
        assert_eq!(reader.advance(0).unwrap(), BackupReadStep::Complete);
        assert_eq!(reader.verified().unwrap().root(), root);
    }
}

#[test]
fn owned_backup_reader_drop_and_failure_preserve_independent_root_leases() {
    let directory = tempfile::tempdir().unwrap();
    let setup = control();
    let repository = LocalRepository::create(
        directory.path().join("backup"),
        protected(),
        limits(),
        &setup,
    )
    .unwrap();
    for failure in 0..3 {
        let root = publish(&repository, &setup);
        let control = control();
        let first = repository
            .open_backup(root.backup(), &control)
            .unwrap()
            .into_image_reader(1 << 20, &control)
            .unwrap();
        let mut second = repository
            .open_backup(root.backup(), &control)
            .unwrap()
            .into_image_reader(1 << 20, &control)
            .unwrap();
        drop(first);
        assert!(matches!(
            repository.remove_backup(root.backup(), &setup),
            Err(BackupError::Busy)
        ));
        assert_eq!(second.advance(7).unwrap(), BackupReadStep::Unit);
        if failure == 0 {
            drop(second);
        } else {
            let error = if failure == 1 {
                control.cancellation().cancel();
                second.read_current(&mut [0; 7]).unwrap_err()
            } else {
                second.advance(7).unwrap_err()
            };
            assert!(matches!(
                error,
                BackupError::Cancelled(_) | BackupError::Invalid(_)
            ));
            assert!(second.verified().is_none());
            assert!(matches!(second.advance(7), Err(BackupError::Finished)));
            // A terminal handle cannot retain a root merely because it remains in scope.
            repository.remove_backup(root.backup(), &setup).unwrap();
            assert_eq!(control.memory().used(), 0);
            continue;
        }
        assert_eq!(control.memory().used(), 0);
        repository.remove_backup(root.backup(), &setup).unwrap();
    }
}

#[test]
fn owned_backup_reader_construction_errors_release_the_moved_root_lease() {
    let directory = tempfile::tempdir().unwrap();
    let setup = control();
    let repository = LocalRepository::create(
        directory.path().join("backup"),
        protected(),
        limits(),
        &setup,
    )
    .unwrap();
    for failure in 0..3 {
        let root = publish(&repository, &setup);
        let lease = repository.open_backup(root.backup(), &setup).unwrap();
        let control = StorageReadControl::with_limit(if failure == 0 { 0 } else { 128 << 10 });
        if failure == 1 {
            control.cancellation().cancel();
        }
        if failure == 2 {
            assert!(matches!(
                crate::key_value::KeyValueImageImportReader::new(
                    lease,
                    ImageProvider::Redb,
                    1 << 20,
                    &control
                ),
                Err(BackupError::Unsupported("record import image format"))
            ));
        } else {
            assert!(lease.into_image_reader(1 << 20, &control).is_err());
        }
        assert_eq!(control.memory().used(), 0);
        repository.remove_backup(root.backup(), &setup).unwrap();
    }
}
