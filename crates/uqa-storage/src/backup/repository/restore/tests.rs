//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::io::{Read, Write};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use crate::backup::*;

use super::super::Fault;
use super::*;

mod paths;
mod scheduled;

struct Fixture {
    directory: tempfile::TempDir,
    repository: LocalRepository,
    backup: BackupId,
    control: StorageReadControl,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let control = StorageReadControl::with_limit(128 * 1024);
        let limits = BackupLimits::new(4096, 4096).unwrap();
        let protection = Protection::Encrypted {
            domain: ProtectionId::from_bytes([1; 16]).unwrap(),
            key: BackupKey::new([47; 32]),
        };
        let repository = LocalRepository::create(
            directory.path().join("repository"),
            protection,
            limits,
            &control,
        )
        .unwrap();
        let cut = BackupCut {
            source: DatabaseId::from_bytes([2; 16]),
            snapshot: SnapshotId::new().unwrap(),
            format: ImageFormat::new(ImageProvider::Memory, 1).unwrap(),
            sequence: None,
        };
        let mut operation = repository
            .begin_backup(cut, None, 1024 * 1024, &control)
            .unwrap();
        let writer = InventoryWriter::new(Vec::new(), cut.format, limits, &control).unwrap();
        let (bytes, summary) = writer.finish().unwrap();
        let span = operation
            .write_object(
                FrameKind::Inventory,
                &mut std::io::Cursor::new(bytes),
                &control,
            )
            .unwrap();
        operation.prepare(span, summary, &control).unwrap();
        let backup = operation.publish(&control).unwrap().backup();
        drop(operation);
        Self {
            directory,
            repository,
            backup,
            control,
        }
    }

    fn path(&self) -> std::path::PathBuf {
        self.directory.path().join("database")
    }

    fn begin(&self) -> BackupRestoreOperation<'_> {
        self.repository
            .begin_restore(self.backup, self.path(), &self.control)
            .unwrap()
    }
}

#[derive(Clone, Default)]
struct Importer {
    imported: Arc<AtomicUsize>,
    recovered: Arc<AtomicUsize>,
    fail_after_import: bool,
}

impl BackupRestoreImporter for Importer {
    fn import(
        self,
        directory: &RestoreDirectory<'_>,
        backup: &BackupLease<'_>,
        request: DatabaseRestore,
        control: &StorageReadControl,
    ) -> BackupResult<()> {
        self.imported.fetch_add(1, Ordering::SeqCst);
        backup.verify(1024 * 1024, control)?;
        assert_ne!(request.source(), request.target());
        let mut file = directory.create_file("database")?;
        file.write_all(&request.target().as_bytes())?;
        file.write_all(b"literal imported contents")?;
        file.sync_all()?;
        if self.fail_after_import {
            return Err(io::Error::other("lost provider completion reply").into());
        }
        Ok(())
    }

    fn recover(
        self,
        directory: &RestoreDirectory<'_>,
        backup: &BackupLease<'_>,
        request: DatabaseRestore,
        control: &StorageReadControl,
    ) -> BackupResult<()> {
        self.recovered.fetch_add(1, Ordering::SeqCst);
        backup.verify(1024 * 1024, control)?;
        let mut file = directory.open_file("database", true)?;
        let mut identity = [0; 16];
        file.read_exact(&mut identity)?;
        if identity != request.target().as_bytes() {
            return Err(BackupError::Integrity);
        }
        file.sync_all()?;
        Ok(())
    }
}

#[test]
fn restore_persists_external_identity_and_preserves_later_writes_after_lost_reply() {
    let fixture = Fixture::new();
    let importer = Importer::default();
    let mut restore = fixture.begin();
    let id = restore.id();
    let request = restore.request();
    assert!(!fixture.path().exists());
    assert!(matches!(
        fixture
            .repository
            .resume_restore(id, fixture.path(), &fixture.control),
        Err(BackupError::Busy)
    ));
    assert!(matches!(restore.publish(), Err(BackupError::Finished)));
    let stored = record::read(
        &fixture.repository,
        &restore.directory,
        id,
        "intent",
        FrameKind::RestoreIntent,
        &fixture.control,
    )
    .unwrap()
    .unwrap();
    assert!(Intent::decode(&stored).unwrap().request == request);
    drop(stored);
    restore.prepare(importer.clone()).unwrap();
    assert!(!fixture.path().exists());
    *fixture.repository.fault.lock() = Some(Fault::RestoreSync);
    assert!(restore.publish().is_err());
    assert!(fixture.path().join("database").is_file());
    std::fs::write(fixture.path().join("database"), b"later committed contents").unwrap();
    drop(restore);
    fixture.control.cancellation().cancel();
    let mut restore = fixture
        .repository
        .resume_restore(id, fixture.path(), &fixture.control)
        .unwrap();
    assert_eq!(restore.request(), request);
    assert_eq!(
        restore.publish().unwrap(),
        RestoredDatabase {
            restore: id,
            backup: fixture.backup,
            database: request.target()
        }
    );
    assert_eq!(restore.cancel().unwrap(), restore.completed());
    restore.prepare(importer.clone()).unwrap();
    assert_eq!(
        std::fs::read(fixture.path().join("database")).unwrap(),
        b"later committed contents"
    );
    assert_eq!(importer.imported.load(Ordering::SeqCst), 1);
    assert_eq!(importer.recovered.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.control.memory().used(), 0);
}

#[test]
fn restore_recovers_provider_outcome_without_repeating_import() {
    let fixture = Fixture::new();
    let importer = Importer {
        fail_after_import: true,
        ..Default::default()
    };
    let mut restore = fixture.begin();
    let id = restore.id();
    let request = restore.request();
    assert!(restore.prepare(importer.clone()).is_err());
    assert!(!fixture.path().exists());
    drop(restore);
    assert_eq!(fixture.control.memory().used(), 0);
    let mut restore = fixture
        .repository
        .resume_restore(id, fixture.path(), &fixture.control)
        .unwrap();
    assert_eq!(restore.request(), request);
    restore.prepare(importer.clone()).unwrap();
    restore.publish().unwrap();
    assert_eq!(importer.imported.load(Ordering::SeqCst), 1);
    assert_eq!(importer.recovered.load(Ordering::SeqCst), 1);
    assert_eq!(
        &std::fs::read(fixture.path().join("database")).unwrap()[16..],
        b"literal imported contents"
    );
    assert_eq!(fixture.control.memory().used(), 0);
}

#[test]
fn restore_prepared_images_survive_archive_loss_and_publication_faults() {
    for fault in [
        Fault::RestorePrepared,
        Fault::RestoreInstall,
        Fault::RestoreComplete,
    ] {
        let fixture = Fixture::new();
        let importer = Importer::default();
        let mut restore = fixture.begin();
        let id = restore.id();
        if fault == Fault::RestorePrepared {
            *fixture.repository.fault.lock() = Some(fault);
            assert!(restore.prepare(importer.clone()).is_err());
        } else {
            restore.prepare(importer.clone()).unwrap();
            *fixture.repository.fault.lock() = Some(fault);
            assert!(restore.publish().is_err());
        }
        drop(restore);
        let root_name = directory::name(fixture.backup.as_bytes());
        fixture
            .repository
            .roots
            .remove_file(directory::text(&root_name))
            .unwrap();
        let mut restore = fixture
            .repository
            .resume_restore(id, fixture.path(), &fixture.control)
            .unwrap();
        restore.prepare(importer.clone()).unwrap();
        let receipt = restore.publish().unwrap();
        assert_eq!(receipt.restore, id);
        assert_eq!(importer.imported.load(Ordering::SeqCst), 1);
        assert_eq!(importer.recovered.load(Ordering::SeqCst), 0);
        assert_eq!(fixture.control.memory().used(), 0);
    }
}

#[test]
fn restore_cancellation_is_durable_and_cannot_reverse_publication() {
    let fixture = Fixture::new();
    let importer = Importer::default();
    let mut restore = fixture.begin();
    let id = restore.id();
    *fixture.repository.fault.lock() = Some(Fault::RestoreCancel);
    assert!(restore.cancel().is_err());
    assert!(matches!(
        restore.prepare(importer.clone()),
        Err(BackupError::Finished)
    ));
    assert!(matches!(restore.publish(), Err(BackupError::Finished)));
    assert_eq!(restore.cancel().unwrap(), None);
    drop(restore);
    let mut restore = fixture
        .repository
        .resume_restore(id, fixture.path(), &fixture.control)
        .unwrap();
    assert!(matches!(
        restore.prepare(importer.clone()),
        Err(BackupError::Finished)
    ));
    assert!(matches!(restore.publish(), Err(BackupError::Finished)));
    assert_eq!(restore.cancel().unwrap(), None);
    assert_eq!(importer.imported.load(Ordering::SeqCst), 0);
    assert!(!fixture.path().exists());
    drop(restore);
    let mut restore = fixture.begin();
    restore.prepare(importer).unwrap();
    *fixture.repository.fault.lock() = Some(Fault::RestoreSync);
    assert!(restore.publish().is_err());
    fixture.control.cancellation().cancel();
    assert!(restore.cancel().unwrap().is_some());
    assert!(restore.completed().is_some());
    assert_eq!(fixture.control.memory().used(), 0);
}

#[test]
fn restore_never_overwrites_empty_directories_files_or_symbolic_links() {
    for kind in 0..3 {
        let fixture = Fixture::new();
        let mut restore = fixture.begin();
        restore.prepare(Importer::default()).unwrap();
        match kind {
            0 => std::fs::create_dir(fixture.path()).unwrap(),
            1 => std::fs::write(fixture.path(), b"existing database").unwrap(),
            _ => {
                #[cfg(unix)]
                std::os::unix::fs::symlink("missing", fixture.path()).unwrap();
                #[cfg(windows)]
                std::fs::create_dir(fixture.path()).unwrap();
            }
        }
        assert!(
            matches!(restore.publish(), Err(BackupError::Io(error)) if error.kind() == io::ErrorKind::AlreadyExists)
        );
        assert!(fixture
            .repository
            .begin_restore(fixture.backup, fixture.path(), &fixture.control)
            .is_err());
        if kind == 1 {
            assert_eq!(std::fs::read(fixture.path()).unwrap(), b"existing database");
        }
        restore.cancel().unwrap();
        assert!(std::fs::symlink_metadata(fixture.path()).is_ok());
        assert_eq!(fixture.control.memory().used(), 0);
    }
}

#[test]
fn restore_validates_staging_identity_and_certificate_before_publication() {
    for substitution in [true, false] {
        let fixture = Fixture::new();
        let mut restore = fixture.begin();
        restore.prepare(Importer::default()).unwrap();
        let (holder, image) = restore.stage().unwrap();
        if substitution {
            drop(image);
            holder.rename("image", &holder, "original").unwrap();
            directory::create_directory(&holder, "image").unwrap();
        } else {
            image.remove_file(MARKER).unwrap();
            directory::create_file(&image, MARKER)
                .unwrap()
                .write_all(b"truncated marker")
                .unwrap();
        }
        assert!(restore.publish().is_err());
        assert!(!fixture.path().exists());
        assert!(restore.completed().is_none());
    }
}

#[cfg(unix)]
#[test]
fn restore_pins_destination_parent_and_rejects_replaced_resume_paths() {
    let fixture = Fixture::new();
    let parent = fixture.directory.path().join("parent");
    let moved = fixture.directory.path().join("moved");
    std::fs::create_dir(&parent).unwrap();
    let mut restore = fixture
        .repository
        .begin_restore(fixture.backup, parent.join("target"), &fixture.control)
        .unwrap();
    let id = restore.id();
    restore.prepare(Importer::default()).unwrap();
    std::fs::rename(&parent, &moved).unwrap();
    std::fs::create_dir(&parent).unwrap();
    restore.publish().unwrap();
    assert!(!parent.join("target").exists());
    assert!(moved.join("target/database").is_file());
    drop(restore);
    assert!(fixture
        .repository
        .resume_restore(id, parent.join("target"), &fixture.control)
        .is_err());
    assert!(fixture
        .repository
        .resume_restore(id, moved.join("target"), &fixture.control)
        .unwrap()
        .completed()
        .is_some());
    assert!(fixture
        .repository
        .resume_restore(id, moved.join("other"), &fixture.control)
        .is_err());
}

#[test]
fn restore_started_intent_cannot_be_reinterpreted_as_prepared_or_fresh() {
    let fixture = Fixture::new();
    let importer = Importer::default();
    let mut restore = fixture.begin();
    let id = restore.id();
    *fixture.repository.fault.lock() = Some(Fault::RestoreStarted);
    assert!(restore.prepare(importer.clone()).is_err());
    drop(restore);
    let mut restore = fixture
        .repository
        .resume_restore(id, fixture.path(), &fixture.control)
        .unwrap();
    assert!(restore.prepare(importer.clone()).is_err());
    assert_eq!(importer.imported.load(Ordering::SeqCst), 0);
    assert_eq!(importer.recovered.load(Ordering::SeqCst), 1);
    let (_, image) = restore.stage().unwrap();
    restore
        .directory
        .hard_link("started", &image, MARKER)
        .unwrap();
    drop(image);
    drop(restore);
    assert!(fixture
        .repository
        .resume_restore(id, fixture.path(), &fixture.control)
        .is_err());
    assert!(!fixture.path().exists());
    assert_eq!(fixture.control.memory().used(), 0);
}

#[test]
fn completed_restore_never_recreates_a_removed_destination() {
    let fixture = Fixture::new();
    let mut restore = fixture.begin();
    let id = restore.id();
    restore.prepare(Importer::default()).unwrap();
    restore.publish().unwrap();
    drop(restore);
    std::fs::remove_dir_all(fixture.path()).unwrap();
    assert!(fixture
        .repository
        .resume_restore(id, fixture.path(), &fixture.control)
        .is_err());
    assert!(!fixture.path().exists());
    assert_eq!(fixture.control.memory().used(), 0);
}

#[test]
fn restore_directory_rejects_archive_paths_and_reserved_markers() {
    let fixture = Fixture::new();
    let restore = fixture.begin();
    let (_, image) = restore.stage().unwrap();
    let directory = RestoreDirectory { directory: &image };
    for name in [
        "../escaped",
        "nested/file",
        "/absolute",
        ".",
        "..",
        MARKER,
        "null\0byte",
    ] {
        assert!(directory.create_file(name).is_err());
        assert!(directory.open_file(name, true).is_err());
    }
    assert!(!fixture.directory.path().join("escaped").exists());
}

#[test]
fn restore_resolves_an_unacknowledged_cancellation_record_before_returning() {
    let fixture = Fixture::new();
    let mut restore = fixture.begin();
    let id = restore.id();
    *fixture.repository.fault.lock() = Some(Fault::RestoreRecordSync);
    assert!(restore.cancel().is_err());
    assert!(restore
        .has_record("cancelled", FrameKind::RestoreCancelled)
        .unwrap());
    drop(restore);
    let mut restore = fixture
        .repository
        .resume_restore(id, fixture.path(), &fixture.control)
        .unwrap();
    assert_eq!(restore.cancel().unwrap(), None);
    assert!(matches!(restore.publish(), Err(BackupError::Finished)));
    assert!(!fixture.path().exists());
    assert_eq!(fixture.control.memory().used(), 0);
}

#[test]
fn restore_cannot_downgrade_uncertain_completion_after_target_removal() {
    let fixture = Fixture::new();
    let mut restore = fixture.begin();
    let id = restore.id();
    restore.prepare(Importer::default()).unwrap();
    *fixture.repository.fault.lock() = Some(Fault::RestoreRecordSync);
    assert!(restore.publish().is_err());
    assert!(restore
        .has_record("complete", FrameKind::RestoreComplete)
        .unwrap());
    std::fs::remove_dir_all(fixture.path()).unwrap();
    assert!(restore.cancel().is_err());
    assert!(!restore
        .has_record("cancelled", FrameKind::RestoreCancelled)
        .unwrap());
    drop(restore);
    assert!(fixture
        .repository
        .resume_restore(id, fixture.path(), &fixture.control)
        .is_err());
    assert!(!fixture.path().exists());
}

#[test]
fn restore_process_exit_releases_admission_and_retains_prepared_publication() {
    const CHILD_DIRECTORY: &str = "UQA_RESTORE_TEST_CHILD_DIRECTORY";
    if let Some(path) = std::env::var_os(CHILD_DIRECTORY) {
        let path = std::path::PathBuf::from(path);
        let control = StorageReadControl::with_limit(128 << 10);
        let repository = LocalRepository::open(
            path.join("repository"),
            Protection::Encrypted {
                domain: ProtectionId::from_bytes([1; 16]).unwrap(),
                key: BackupKey::new([47; 32]),
            },
            BackupLimits::new(4096, 4096).unwrap(),
            &control,
        )
        .unwrap();
        let id = BackupId::from_bytes(
            std::fs::read(path.join("backup-id"))
                .unwrap()
                .try_into()
                .unwrap(),
        )
        .unwrap();
        let mut restore = repository
            .begin_restore(id, path.join("database"), &control)
            .unwrap();
        let mut file = File::create(path.join("restore-id")).unwrap();
        file.write_all(&restore.id().as_bytes()).unwrap();
        file.sync_all().unwrap();
        restore.prepare(Importer::default()).unwrap();
        // Exit without running the restore owner's Drop; the operating system must retire admission.
        std::process::exit(0);
    }
    let fixture = Fixture::new();
    std::fs::write(
        fixture.directory.path().join("backup-id"),
        fixture.backup.as_bytes(),
    )
    .unwrap();
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "backup::repository::restore::tests::restore_process_exit_releases_admission_and_retains_prepared_publication"])
        .env(CHILD_DIRECTORY, fixture.directory.path()).output().unwrap();
    assert!(
        child.status.success(),
        "{}",
        String::from_utf8_lossy(&child.stderr)
    );
    assert!(!fixture.path().exists());
    let id = RestoreId::from_bytes(
        std::fs::read(fixture.directory.path().join("restore-id"))
            .unwrap()
            .try_into()
            .unwrap(),
    )
    .unwrap();
    let mut restore = fixture
        .repository
        .resume_restore(id, fixture.path(), &fixture.control)
        .unwrap();
    restore.publish().unwrap();
    assert_eq!(
        &std::fs::read(fixture.path().join("database")).unwrap()[16..],
        b"literal imported contents"
    );
    assert_eq!(fixture.control.memory().used(), 0);
}
