//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A redb destination retains its pinned private file until the enclosing catalog owner validates it.

use crate::{RedbBackupImport, RedbBackupImportStep, RedbStorage};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use uqa_storage::{
    backup::{
        BackupError, BackupImageImportStep, BackupImageValidationStep, BackupImageValidator,
        BackupImportLimits, BackupLease, BackupRestoreWorkStep, BackupResult, BackupRoot,
        ImageProvider, OwnedBackupImageValidation, OwnedPersistentBackupImport,
        PersistentBackupImport, PersistentBackupTarget, RestoreDirectory, RestoreFileWork,
    },
    mvcc::{DatabaseRestore, VersionedSessionOptions},
    read_control::StorageReadControl,
    PersistentStorageProvider,
};

const DATABASE: &str = "database";

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
        OwnedPersistentBackupImport::create(control, || {
            let file = if recover {
                None
            } else {
                Some(directory.create_file(DATABASE)?)
            };
            let work = directory.begin_file_work(DATABASE, control, |_| {
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
                    validation: None,
                    root,
                    request,
                    target: self,
                    control: control.clone(),
                })
            })?;
            Ok(PinnedImport(work))
        })
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
        let capacity = directory
            .as_os_str()
            .len()
            .checked_add(DATABASE.len() + 1)
            .ok_or(BackupError::Invalid("restore pathname length"))?;
        let _memory = control.memory().reserve(capacity)?;
        let mut path = PathBuf::with_capacity(capacity);
        path.push(directory);
        path.push(DATABASE);
        Ok(Arc::new(RedbStorage::open_restored(
            path,
            request,
            self.session_options,
            control,
        )?))
    }
}

struct Import<'a> {
    // An absent input denotes recovery of an already verified physical image. The enclosing owned importer admits validation only after this boundary succeeds.
    input: Option<RedbBackupImport<'a>>,
    validation: Option<OwnedBackupImageValidation>,
    root: BackupRoot,
    request: DatabaseRestore,
    target: RedbBackupTarget,
    control: StorageReadControl,
}

struct PinnedImport<'a>(RestoreFileWork<Import<'a>>);

impl PersistentBackupImport for PinnedImport<'_> {
    fn advance(&mut self, maximum_object_bytes: usize) -> BackupResult<BackupImageImportStep> {
        let mut result = BackupImageImportStep::Pending;
        self.0.advance_with(|owner, path| {
            owner.control.check()?;
            if let Some(mut input) = owner.input.take() {
                if input.advance_with_window(maximum_object_bytes)? == RedbBackupImportStep::Pending
                {
                    owner.input = Some(input);
                    return Ok(BackupRestoreWorkStep::Pending);
                }
                drop(input);
            } else {
                RedbBackupImport::recover(path, owner.root, owner.request, &owner.control)?;
            }
            result = BackupImageImportStep::Imported;
            Ok(BackupRestoreWorkStep::Pending)
        })?;
        Ok(result)
    }

    fn validate(&mut self, validator: &mut dyn BackupImageValidator) -> BackupResult<()> {
        while self.advance_validation(validator, 64)? == BackupImageValidationStep::Pending {}
        Ok(())
    }

    fn advance_validation(
        &mut self,
        validator: &mut dyn BackupImageValidator,
        maximum_steps: usize,
    ) -> BackupResult<BackupImageValidationStep> {
        let step = self.0.advance_with(|owner, path| {
            if owner.input.is_some() {
                return Err(BackupError::Invalid("redb image import is incomplete"));
            }
            let mut validation = if let Some(validation) = owner.validation.take() {
                validation
            } else {
                let provider = RedbStorage::open_restored(
                    path,
                    owner.request,
                    owner.target.session_options,
                    &owner.control,
                )?;
                validator.begin(Arc::new(provider), &owner.control)?
            };
            match validation.advance(maximum_steps)? {
                BackupImageValidationStep::Pending => {
                    owner.validation = Some(validation);
                    Ok(BackupRestoreWorkStep::Pending)
                }
                BackupImageValidationStep::Complete => Ok(BackupRestoreWorkStep::Complete),
            }
        })?;
        Ok(match step {
            BackupRestoreWorkStep::Pending => BackupImageValidationStep::Pending,
            BackupRestoreWorkStep::Complete => BackupImageValidationStep::Complete,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(!directory.path().join(DATABASE).exists());
        assert_eq!(control.memory().used(), 0);
    }
}
