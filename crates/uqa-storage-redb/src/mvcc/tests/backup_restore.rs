//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Repository restore admission composes with the actual redb image and history owners.

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use redb::ReadableDatabase;
use uqa_storage::{
    backup::{BackupError, BackupLease, BackupRestoreImporter, BackupResult, RestoreDirectory},
    diskann_index::format::DiskANNGeneration,
    key_value::conformance::{
        verify_diskann_restore_source, verify_diskann_restored, verify_diskann_restored_writes,
    },
    mvcc::{DatabaseRestore, VersionedPersistence, VersionedSessionOptions},
    read_control::StorageReadControl,
    KeyValueStorageBackend, KeyValueStore, PersistentStorageProvider,
};

use crate::{RedbBackupImport, RedbBackupImportStep, RedbKeyValueStore, RedbStorage};

#[derive(Clone)]
struct FixtureImporter {
    generation: DiskANNGeneration,
    imported: Arc<AtomicUsize>,
    recovered: Arc<AtomicUsize>,
    lose_reply: bool,
}

impl FixtureImporter {
    fn validate(
        &self,
        directory: &RestoreDirectory<'_>,
        backup: &BackupLease<'_>,
        request: DatabaseRestore,
        control: &StorageReadControl,
    ) -> BackupResult<()> {
        let file = directory.open_file("database", true)?;
        let database = redb::Database::builder()
            .create_file(file)
            .map_err(crate::error::redb_error)?;
        {
            let transaction = database.begin_read().map_err(crate::error::redb_error)?;
            let metadata = transaction
                .open_table(super::super::METADATA)
                .map_err(crate::error::redb_error)?;
            let root = metadata
                .get("backup_image")
                .map_err(crate::error::redb_error)?;
            if root.as_ref().map(redb::AccessGuard::value)
                != Some(backup.root().encode().as_slice())
            {
                return Err(BackupError::Integrity);
            }
        }
        let records = super::super::restore::open(database, request, control)?;
        if records.database_id() != request.target() {
            return Err(BackupError::Integrity);
        }
        let store =
            RedbKeyValueStore::new(Arc::new(records), None, VersionedSessionOptions::default());
        let backend = KeyValueStorageBackend::new(Arc::new(store));
        verify_diskann_restored(&backend, self.generation)?;
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
        let mut importer = RedbBackupImport::new(
            directory.create_file("database")?,
            backup,
            request,
            256 << 10,
            2 << 20,
            control,
        )?;
        while importer.advance()? != RedbBackupImportStep::Complete {}
        drop(importer);
        self.validate(directory, backup, request, control)?;
        if self.lose_reply {
            return Err(std::io::Error::other("lost completed import reply").into());
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
        self.validate(directory, backup, request, control)
    }
}

#[test]
fn redb_repository_restore_recovers_closed_import_and_preserves_published_writes() {
    let temporary = tempfile::tempdir().unwrap();
    let source_path = temporary.path().join("source.redb");
    let target_path = temporary.path().join("restored");
    let source = RedbStorage::open(&source_path).unwrap();
    let session = source.open_session().unwrap();
    let generation = verify_diskann_restore_source(&*session.backend).unwrap();
    drop(session);
    source
        .store()
        .put(b"literal", b"source-independent bytes")
        .unwrap();
    let control = StorageReadControl::with_limit(384 << 10);
    let (repository, root) =
        super::backup_import::capture(&source, &temporary.path().join("repository"), &control);
    drop(source);
    std::fs::remove_file(source_path).unwrap();
    let importer = FixtureImporter {
        generation,
        imported: Arc::default(),
        recovered: Arc::default(),
        lose_reply: true,
    };
    let mut restore = repository
        .begin_restore(root.backup(), &target_path, &control)
        .unwrap();
    let id = restore.id();
    let request = restore.request();
    assert!(restore.prepare(importer.clone()).is_err());
    assert!(!target_path.exists());
    drop(restore);
    assert_eq!(control.memory().used(), 0);
    let mut restore = repository
        .resume_restore(id, &target_path, &control)
        .unwrap();
    restore.prepare(importer.clone()).unwrap();
    assert!(!target_path.exists());
    let receipt = restore.publish().unwrap();
    assert_eq!(receipt.database, request.target());
    let owner = RedbStorage::open(target_path.join("database")).unwrap();
    assert_eq!(
        owner.record_store().unwrap().database_id(),
        request.target()
    );
    assert_eq!(
        owner.store().get(b"literal").unwrap().unwrap(),
        b"source-independent bytes"
    );
    let session = owner.open_session().unwrap();
    verify_diskann_restored_writes(&*session.backend, generation).unwrap();
    owner
        .store()
        .put(b"later", b"must remain after retry")
        .unwrap();
    drop((owner, session, restore));
    control.cancellation().cancel();
    let mut restore = repository
        .resume_restore(id, &target_path, &control)
        .unwrap();
    assert_eq!(restore.publish().unwrap(), receipt);
    restore.prepare(importer.clone()).unwrap();
    let owner = RedbStorage::open(target_path.join("database")).unwrap();
    assert_eq!(
        owner.store().get(b"later").unwrap().unwrap(),
        b"must remain after retry"
    );
    assert_eq!(importer.imported.load(Ordering::SeqCst), 1);
    assert_eq!(importer.recovered.load(Ordering::SeqCst), 1);
    assert_eq!(control.memory().used(), 0);
    assert!(control.memory().peak() <= 384 << 10);
}
