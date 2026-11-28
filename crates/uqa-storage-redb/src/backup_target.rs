//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A redb destination retains its pinned private file until the enclosing catalog owner validates it.

use crate::{RedbBackupImport, RedbBackupImportStep, RedbStorage};
use std::{path::Path, sync::Arc};
use uqa_storage::{
    backup::{
        restored_database_path, BackupError, BackupFileInput, BackupImageImportStep,
        BackupImportLimits, BackupLease, BackupResult, BackupRoot, ImageProvider,
        OwnedPersistentBackupImport, PersistentBackupTarget, RestoreDirectory,
    },
    mvcc::{DatabaseRestore, VersionedSessionOptions},
    read_control::StorageReadControl,
    PersistentStorageProvider,
};

#[derive(Clone, Copy, Debug, Default)]
pub struct RedbBackupTarget {
    pub session_options: VersionedSessionOptions,
}

impl RedbBackupTarget {
    fn start<'a>(
        self,
        directory: &RestoreDirectory<'_>,
        backup: BackupLease<'a>,
        request: DatabaseRestore,
        limits: BackupImportLimits,
        control: &StorageReadControl,
        recover: bool,
    ) -> BackupResult<OwnedPersistentBackupImport<'a>> {
        let root = backup.root();
        if root.cut().format.provider() != ImageProvider::Redb {
            return Err(BackupError::Unsupported("redb restore destination mapping"));
        }
        OwnedPersistentBackupImport::from_file(
            directory,
            recover,
            control,
            |file, _| {
                let input = file
                    .map(|file| {
                        RedbBackupImport::new(
                            file,
                            backup,
                            request,
                            limits.maximum_value_bytes,
                            limits.maximum_workspace_bytes,
                            control,
                        )
                    })
                    .transpose()?;
                Ok(Import {
                    input,
                    root,
                    request,
                })
            },
            move |path, control| self.open_file(path, request, control),
        )
    }

    fn open_file(
        self,
        path: &Path,
        request: DatabaseRestore,
        control: &StorageReadControl,
    ) -> BackupResult<Arc<dyn PersistentStorageProvider>> {
        Ok(Arc::new(RedbStorage::open_restored(
            path,
            request,
            self.session_options,
            control,
        )?))
    }
}

impl PersistentBackupTarget for RedbBackupTarget {
    fn image_provider(&self) -> ImageProvider {
        ImageProvider::Redb
    }

    fn import<'a>(
        &self,
        directory: &RestoreDirectory<'_>,
        backup: BackupLease<'a>,
        request: DatabaseRestore,
        limits: BackupImportLimits,
        control: &StorageReadControl,
    ) -> BackupResult<OwnedPersistentBackupImport<'a>> {
        self.start(directory, backup, request, limits, control, false)
    }

    fn recover<'a>(
        &self,
        directory: &RestoreDirectory<'_>,
        backup: BackupLease<'a>,
        request: DatabaseRestore,
        limits: BackupImportLimits,
        control: &StorageReadControl,
    ) -> BackupResult<OwnedPersistentBackupImport<'a>> {
        self.start(directory, backup, request, limits, control, true)
    }

    fn open(
        &self,
        directory: &Path,
        request: DatabaseRestore,
        control: &StorageReadControl,
    ) -> BackupResult<Arc<dyn PersistentStorageProvider>> {
        let path = restored_database_path(directory, control)?;
        self.open_file(&path, request, control)
    }
}

struct Import<'a> {
    input: Option<RedbBackupImport<'a>>,
    root: BackupRoot,
    request: DatabaseRestore,
}

impl BackupFileInput for Import<'_> {
    fn advance(
        &mut self,
        path: &Path,
        maximum_object_bytes: usize,
        control: &StorageReadControl,
    ) -> BackupResult<BackupImageImportStep> {
        if let Some(input) = &mut self.input {
            if input.advance_with_window(maximum_object_bytes)? == RedbBackupImportStep::Pending {
                return Ok(BackupImageImportStep::Pending);
            }
        } else {
            RedbBackupImport::recover(path, self.root, self.request, control)?;
        }
        Ok(BackupImageImportStep::Imported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_view_queries_read_the_original_physical_cut() {
        let directory = tempfile::tempdir().unwrap();
        let provider = RedbStorage::open(directory.path().join("views")).unwrap();
        uqa_storage::key_value::conformance::verify_backup_view_queries(&provider).unwrap();
    }

    #[test]
    fn stored_metadata_values_read_the_original_physical_cut() {
        let directory = tempfile::tempdir().unwrap();
        let provider = RedbStorage::open(directory.path().join("metadata")).unwrap();
        uqa_storage::key_value::conformance::verify_backup_metadata_values(&provider).unwrap();
    }

    #[test]
    fn redb_destination_retains_validation_until_completion_and_recovers_cancellation() {
        let directory = tempfile::tempdir().unwrap();
        let provider = Arc::new(RedbStorage::open(directory.path().join("source")).unwrap());
        let target: Arc<dyn PersistentBackupTarget> = Arc::new(RedbBackupTarget::default());
        uqa_storage::key_value::conformance::verify_backup_validation_continuation(
            provider,
            &target,
            directory.path(),
        )
        .unwrap();
    }

    #[test]
    fn redb_restore_target_never_creates_a_missing_completed_database() {
        let directory = tempfile::tempdir().unwrap();
        let control = StorageReadControl::with_limit(1 << 20);
        let request =
            DatabaseRestore::new(uqa_storage::mvcc::DatabaseId::from_bytes([31; 16])).unwrap();
        assert!(RedbBackupTarget::default()
            .open(directory.path(), request, &control)
            .is_err());
        assert!(!directory.path().join("database").exists());
        assert_eq!(control.memory().used(), 0);
    }
}
