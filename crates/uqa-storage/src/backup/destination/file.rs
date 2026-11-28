//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! File-backed destinations retain one private file binding from physical import through logical validation.

use super::{
    BackupError, BackupImageImportStep, BackupImageValidationStep, BackupImageValidator,
    BackupResult, OwnedBackupImageValidation, OwnedPersistentBackupImport, PersistentBackupImport,
    PersistentStorageProvider, RestoreDirectory, StorageReadControl,
};
use crate::backup::{BackupRestoreWorkStep, RestoreFileWork};
use std::{
    fs::File,
    path::{Path, PathBuf},
    sync::Arc,
};
use uqa_core::memory::Budgeted;

const DATABASE_FILE: &str = "database";

/// The installed file's pathname and its allowance for opening a prepared or published database.
pub fn restored_database_path(
    directory: &Path,
    control: &StorageReadControl,
) -> BackupResult<Budgeted<PathBuf>> {
    let capacity = directory
        .as_os_str()
        .len()
        .checked_add(DATABASE_FILE.len() + 1)
        .ok_or(BackupError::Invalid("restore pathname length"))?;
    let memory = control.memory().reserve(capacity)?;
    let mut path = PathBuf::with_capacity(capacity);
    path.push(directory);
    path.push(DATABASE_FILE);
    Ok(Budgeted::new(path, memory))
}

/// Physical work on a pinned file. Returning Imported releases the input and its handles before the validator opens that same file.
pub trait BackupFileInput {
    fn advance(
        &mut self,
        path: &Path,
        maximum_object_bytes: usize,
        control: &StorageReadControl,
    ) -> BackupResult<BackupImageImportStep>;
}

struct FileState<I, O> {
    input: Option<I>,
    open: O,
    validation: Option<OwnedBackupImageValidation>,
}

struct PinnedImport<I, O> {
    work: RestoreFileWork<FileState<I, O>>,
    control: StorageReadControl,
}

impl<'a> OwnedPersistentBackupImport<'a> {
    /// Admit the importer before creating a fresh file. Recovery opens the existing private file. Both paths retain its directory binding until the validator finishes.
    pub fn from_file<I: BackupFileInput + 'a>(
        directory: &RestoreDirectory<'_>,
        recover: bool,
        control: &StorageReadControl,
        start: impl FnOnce(Option<File>, &Path) -> BackupResult<I>,
        open: impl Fn(&Path, &StorageReadControl) -> BackupResult<Arc<dyn PersistentStorageProvider>>
            + 'a,
    ) -> BackupResult<Self> {
        Self::create(control, || {
            let file = if recover {
                None
            } else {
                Some(directory.create_file(DATABASE_FILE)?)
            };
            let work = directory.begin_file_work(DATABASE_FILE, control, |path| {
                Ok(FileState {
                    input: Some(start(file, path)?),
                    open,
                    validation: None,
                })
            })?;
            Ok(PinnedImport {
                work,
                control: control.clone(),
            })
        })
    }
}

impl<I: BackupFileInput, O> PersistentBackupImport for PinnedImport<I, O>
where
    O: Fn(&Path, &StorageReadControl) -> BackupResult<Arc<dyn PersistentStorageProvider>>,
{
    fn advance(&mut self, maximum_object_bytes: usize) -> BackupResult<BackupImageImportStep> {
        let mut step = BackupImageImportStep::Pending;
        self.work.advance_with(|state, path| {
            self.control.check()?;
            let input = state.input.as_mut().ok_or(BackupError::Finished)?;
            step = input.advance(path, maximum_object_bytes, &self.control)?;
            if step == BackupImageImportStep::Imported {
                state.input = None;
            }
            Ok(BackupRestoreWorkStep::Pending)
        })?;
        Ok(step)
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
        let step = self.work.advance_with(|state, path| {
            let validation = match &mut state.validation {
                Some(validation) => validation,
                None => state
                    .validation
                    .insert(validator.begin((state.open)(path, &self.control)?, &self.control)?),
            };
            Ok(match validation.advance(maximum_steps)? {
                BackupImageValidationStep::Pending => BackupRestoreWorkStep::Pending,
                BackupImageValidationStep::Complete => BackupRestoreWorkStep::Complete,
            })
        })?;
        Ok(match step {
            BackupRestoreWorkStep::Pending => BackupImageValidationStep::Pending,
            BackupRestoreWorkStep::Complete => BackupImageValidationStep::Complete,
        })
    }
}
