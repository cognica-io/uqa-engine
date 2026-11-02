//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Actual `SQLite` images compose with pinned repository staging and exclusive publication.

use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

use uqa_storage::{
    backup::{BackupError, BackupLease, BackupRestoreImporter, BackupResult, RestoreDirectory},
    diskann_index::format::DiskANNGeneration,
    key_value::conformance::{
        verify_diskann_restore_source, verify_diskann_restored, verify_diskann_restored_writes,
    },
    mvcc::{DatabaseRestore, VersionedPersistence},
    read_control::StorageReadControl,
    KeyValueStore,
};

use super::{
    backup_import::{capture, options},
    open,
};
use crate::{
    ManagedConnection, SQLiteBackupImportStep, SQLiteError, SQLiteKeyValueBackupImport,
    SQLiteKeyValueStorage, SQLiteRecordStore,
};

fn sqlite_error(error: SQLiteError) -> BackupError {
    uqa_storage::StorageBackendError::from(error).into()
}

#[derive(Clone)]
struct FixtureImporter {
    mode: usize,
    generation: DiskANNGeneration,
    imported: Arc<AtomicUsize>,
    recovered: Arc<AtomicUsize>,
    lose_reply: bool,
    movement: Option<(PathBuf, PathBuf)>,
}

impl FixtureImporter {
    fn validate(
        &self,
        path: &Path,
        request: DatabaseRestore,
        control: &StorageReadControl,
    ) -> BackupResult<()> {
        let connection = open(path, self.mode).map_err(sqlite_error)?;
        let records = SQLiteRecordStore::for_key_value(&connection, control)?;
        if records.database_id() != request.target() {
            return Err(BackupError::Integrity);
        }
        let owner = SQLiteKeyValueStorage::from_connection(connection).map_err(sqlite_error)?;
        verify_diskann_restored(&owner.backend(), self.generation)?;
        Ok(())
    }
}

impl BackupRestoreImporter for FixtureImporter {
    fn import(
        self,
        directory: &RestoreDirectory<'_>,
        backup: &BackupLease<'_>,
        request: DatabaseRestore,
        control: &StorageReadControl,
    ) -> BackupResult<()> {
        self.imported.fetch_add(1, Ordering::SeqCst);
        drop(directory.create_file("database")?);
        directory.with_file_path("database", control, |path| {
            let mut import = SQLiteKeyValueBackupImport::new(
                path,
                backup,
                request,
                options(self.mode),
                control,
            )?;
            while import.advance()? != SQLiteBackupImportStep::Complete {}
            drop(import);
            self.validate(path, request, control)?;
            if let Some((before, after)) = &self.movement {
                std::fs::rename(before, after)?;
                std::fs::create_dir(before)?;
                std::fs::write(before.join("unrelated"), b"unrelated parent")?;
            }
            if self.lose_reply {
                return Err(std::io::Error::other("lost completed import reply").into());
            }
            Ok(())
        })
    }

    fn recover(
        self,
        directory: &RestoreDirectory<'_>,
        backup: &BackupLease<'_>,
        request: DatabaseRestore,
        control: &StorageReadControl,
    ) -> BackupResult<()> {
        self.recovered.fetch_add(1, Ordering::SeqCst);
        directory.with_file_path("database", control, |path| {
            SQLiteKeyValueBackupImport::recover(
                path,
                backup.root(),
                request,
                options(self.mode),
                control,
            )?;
            self.validate(path, request, control)
        })
    }
}

#[test]
fn sqlite_repository_restore_recovers_closed_import_and_preserves_published_writes() {
    for mode in 0..4 {
        let temporary = tempfile::tempdir().unwrap();
        let source_path = temporary.path().join("source");
        std::fs::create_dir(&source_path).unwrap();
        let source = SQLiteKeyValueStorage::from_connection(
            ManagedConnection::open(&source_path.join("database")).unwrap(),
        )
        .unwrap();
        let generation = verify_diskann_restore_source(&source.backend()).unwrap();
        source
            .store()
            .put(b"literal", b"source-independent bytes")
            .unwrap();
        let control = StorageReadControl::with_limit(384 << 10);
        let (repository, root) = capture(&source, &temporary.path().join("repository"), &control);
        drop(source);
        std::fs::remove_dir_all(&source_path).unwrap();
        let target = temporary.path().join("restored");
        let importer = FixtureImporter {
            mode,
            generation,
            imported: Arc::default(),
            recovered: Arc::default(),
            lose_reply: true,
            movement: None,
        };
        let mut restore = repository
            .begin_restore(root.backup(), &target, &control)
            .unwrap();
        let id = restore.id();
        let request = restore.request();
        assert!(restore.prepare(importer.clone()).is_err());
        assert!(!target.exists());
        drop(restore);
        assert_eq!(control.memory().used(), 0);
        let mut restore = repository.resume_restore(id, &target, &control).unwrap();
        restore.prepare(importer.clone()).unwrap();
        assert!(!target.exists());
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("existing"), b"must not replace").unwrap();
        assert!(restore.publish().is_err());
        assert_eq!(
            std::fs::read(target.join("existing")).unwrap(),
            b"must not replace"
        );
        std::fs::remove_file(target.join("existing")).unwrap();
        std::fs::remove_dir(&target).unwrap();
        let receipt = restore.publish().unwrap();
        assert_eq!(receipt.database, request.target());
        let owner =
            SQLiteKeyValueStorage::from_connection(open(&target.join("database"), mode).unwrap())
                .unwrap();
        assert_eq!(
            owner.store().get(b"literal").unwrap().unwrap(),
            b"source-independent bytes"
        );
        verify_diskann_restored_writes(&owner.backend(), generation).unwrap();
        owner
            .store()
            .put(b"later", b"must remain after retry")
            .unwrap();
        drop((owner, restore));
        control.cancellation().cancel();
        let mut restore = repository.resume_restore(id, &target, &control).unwrap();
        assert_eq!(restore.publish().unwrap(), receipt);
        restore.prepare(importer.clone()).unwrap();
        let owner =
            SQLiteKeyValueStorage::from_connection(open(&target.join("database"), mode).unwrap())
                .unwrap();
        assert_eq!(
            owner.store().get(b"later").unwrap().unwrap(),
            b"must remain after retry"
        );
        assert_eq!(importer.imported.load(Ordering::SeqCst), 1);
        assert_eq!(importer.recovered.load(Ordering::SeqCst), 1);
        assert_eq!(control.memory().used(), 0);
        assert!(control.memory().peak() <= 384 << 10);
    }
}

#[cfg(unix)]
#[test]
fn sqlite_repository_restore_tracks_parent_movement_before_and_during_import() {
    for mode in 0..4 {
        for during in [false, true] {
            let temporary = tempfile::tempdir().unwrap();
            let source = SQLiteKeyValueStorage::from_connection(
                ManagedConnection::open_in_memory().unwrap(),
            )
            .unwrap();
            let generation = verify_diskann_restore_source(&source.backend()).unwrap();
            let control = StorageReadControl::with_limit(384 << 10);
            let (repository, root) =
                capture(&source, &temporary.path().join("repository"), &control);
            drop(source);
            let parent = temporary.path().join("original");
            let moved = temporary.path().join("moved");
            std::fs::create_dir(&parent).unwrap();
            let mut restore = repository
                .begin_restore(root.backup(), parent.join("database"), &control)
                .unwrap();
            let id = restore.id();
            let importer = FixtureImporter {
                mode,
                generation,
                imported: Arc::default(),
                recovered: Arc::default(),
                lose_reply: false,
                movement: during.then(|| (parent.clone(), moved.clone())),
            };
            if during {
                assert!(restore.prepare(importer.clone()).is_err());
                assert!(!moved.join("database").exists());
            } else {
                std::fs::rename(&parent, &moved).unwrap();
                std::fs::create_dir(&parent).unwrap();
                std::fs::write(parent.join("unrelated"), b"unrelated parent").unwrap();
                restore.prepare(importer.clone()).unwrap();
            }
            drop(restore);
            let mut restore = repository
                .resume_restore(id, moved.join("database"), &control)
                .unwrap();
            restore.prepare(importer.clone()).unwrap();
            let receipt = restore.publish().unwrap();
            importer
                .validate(
                    &moved.join("database/database"),
                    restore.request(),
                    &control,
                )
                .unwrap();
            assert_eq!(receipt.database, restore.request().target());
            assert_eq!(
                std::fs::read(parent.join("unrelated")).unwrap(),
                b"unrelated parent"
            );
            assert!(!parent.join("database").exists());
            assert_eq!(importer.imported.load(Ordering::SeqCst), 1);
            assert_eq!(
                importer.recovered.load(Ordering::SeqCst),
                usize::from(during)
            );
            drop(restore);
            assert_eq!(control.memory().used(), 0);
        }
    }
}
