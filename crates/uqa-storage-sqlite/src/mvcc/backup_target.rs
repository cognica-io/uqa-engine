//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native and Key/Value destinations keep physical import and logical validation on one pinned private file.

use super::backup_import::sqlite_error;
use crate::{
    ManagedConnection, SQLiteBackupImportOptions, SQLiteBackupImportStep, SQLiteCompressionOptions,
    SQLiteKeyValueBackupImport, SQLiteKeyValueStorage, SQLiteNativeBackupImport,
    SQLiteStorageProvider,
};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use uqa_core::memory::Budgeted;
use uqa_storage::{
    backup::{
        BackupError, BackupImageImportStep, BackupImageValidationStep, BackupImageValidator,
        BackupImportLimits, BackupLease, BackupRestoreWorkStep, BackupResult, BackupRoot,
        ImageProvider, OwnedBackupImageValidation, OwnedPersistentBackupImport,
        PersistentBackupImport, PersistentBackupTarget, RestoreDirectory, RestoreFileWork,
    },
    mvcc::{DatabaseRestore, VersionedSessionOptions},
    read_control::StorageReadControl,
    PersistentStorageProvider, StorageEncryptionKey,
};

const DATABASE: &str = "database";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SQLiteBackupMapping {
    Native,
    KeyValue,
}

#[derive(Clone, Debug)]
pub struct SQLiteBackupTarget {
    pub mapping: SQLiteBackupMapping,
    pub compression: Option<SQLiteCompressionOptions>,
    pub encryption_key: Option<StorageEncryptionKey>,
    pub session_options: VersionedSessionOptions,
}

impl SQLiteBackupTarget {
    pub fn new(mapping: SQLiteBackupMapping) -> Self {
        Self {
            mapping,
            compression: None,
            encryption_key: None,
            session_options: VersionedSessionOptions::default(),
        }
    }

    fn options(&self, limits: BackupImportLimits) -> SQLiteBackupImportOptions {
        SQLiteBackupImportOptions {
            compression: self.compression,
            encryption_key: self.encryption_key.clone(),
            maximum_value_bytes: limits.maximum_value_bytes,
            maximum_workspace_bytes: limits.maximum_workspace_bytes,
        }
    }

    fn open_file(
        &self,
        path: &Path,
        request: DatabaseRestore,
        control: &StorageReadControl,
    ) -> BackupResult<Arc<dyn PersistentStorageProvider>> {
        control.check()?;
        let key = self
            .encryption_key
            .as_ref()
            .map(StorageEncryptionKey::expose_secret);
        let connection = match (self.compression, key) {
            (None, None) => ManagedConnection::open_restored(path, request, control),
            (None, Some(key)) => {
                ManagedConnection::open_encrypted_restored(path, key, request, control)
            }
            (Some(compression), None) => {
                ManagedConnection::open_compressed_restored(path, compression, request, control)
            }
            (Some(compression), Some(key)) => {
                ManagedConnection::open_compressed_encrypted_restored(
                    path,
                    key,
                    compression,
                    request,
                    control,
                )
            }
        }
        .map_err(sqlite_error)?;
        let provider: Arc<dyn PersistentStorageProvider> = match self.mapping {
            SQLiteBackupMapping::Native => {
                connection
                    .bind_native_records(self.session_options)
                    .map_err(sqlite_error)?;
                Arc::new(SQLiteStorageProvider::new(connection))
            }
            SQLiteBackupMapping::KeyValue => Arc::new(
                SQLiteKeyValueStorage::from_connection_with_options(
                    connection,
                    self.session_options,
                )
                .map_err(sqlite_error)?,
            ),
        };
        control.check()?;
        Ok(provider)
    }

    fn start<'a>(
        &self,
        directory: &RestoreDirectory<'_>,
        backup: BackupLease<'a>,
        request: DatabaseRestore,
        limits: BackupImportLimits,
        control: &StorageReadControl,
        recover: bool,
    ) -> BackupResult<OwnedPersistentBackupImport<'a>> {
        if backup.root().cut().format.provider() != self.image_provider() {
            return Err(BackupError::Unsupported(
                "SQLite restore destination mapping",
            ));
        }
        OwnedPersistentBackupImport::create(control, || {
            if !recover {
                drop(directory.create_file(DATABASE)?);
            }
            let work = directory.begin_file_work(DATABASE, control, |path| {
                let state = if recover {
                    let memory = control.memory().reserve(size_of::<Recovery>())?;
                    State::Recover(Budgeted::new(
                        Box::new(Recovery {
                            root: backup.root(),
                            request,
                            options: self.options(limits),
                        }),
                        memory,
                    ))
                } else {
                    match self.mapping {
                        SQLiteBackupMapping::Native => {
                            let memory = control
                                .memory()
                                .reserve(size_of::<SQLiteNativeBackupImport<'_>>())?;
                            let input = SQLiteNativeBackupImport::new(
                                path,
                                backup,
                                request,
                                self.options(limits),
                                control,
                            )?;
                            State::Native(Budgeted::new(Box::new(input), memory))
                        }
                        SQLiteBackupMapping::KeyValue => {
                            let memory = control
                                .memory()
                                .reserve(size_of::<SQLiteKeyValueBackupImport<'_>>())?;
                            let input = SQLiteKeyValueBackupImport::new(
                                path,
                                backup,
                                request,
                                self.options(limits),
                                control,
                            )?;
                            State::KeyValue(Budgeted::new(Box::new(input), memory))
                        }
                    }
                };
                Ok(Import {
                    target: self.clone(),
                    state,
                    request,
                    control: control.clone(),
                })
            })?;
            Ok(PinnedImport(work))
        })
    }
}

impl PersistentBackupTarget for SQLiteBackupTarget {
    fn image_provider(&self) -> ImageProvider {
        match self.mapping {
            SQLiteBackupMapping::Native => ImageProvider::SQLiteNative,
            SQLiteBackupMapping::KeyValue => ImageProvider::SQLiteKV,
        }
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
        self.open_file(&path, request, control)
    }
}

struct Recovery {
    root: BackupRoot,
    request: DatabaseRestore,
    options: SQLiteBackupImportOptions,
}

enum State<'a> {
    Native(Budgeted<Box<SQLiteNativeBackupImport<'a>>>),
    KeyValue(Budgeted<Box<SQLiteKeyValueBackupImport<'a>>>),
    Recover(Budgeted<Box<Recovery>>),
    Imported,
    Validating(OwnedBackupImageValidation),
    Failed,
}

struct Import<'a> {
    target: SQLiteBackupTarget,
    state: State<'a>,
    request: DatabaseRestore,
    control: StorageReadControl,
}

struct PinnedImport<'a>(RestoreFileWork<Import<'a>>);

impl PersistentBackupImport for PinnedImport<'_> {
    fn advance(&mut self, maximum_object_bytes: usize) -> BackupResult<BackupImageImportStep> {
        let mut result = BackupImageImportStep::Pending;
        self.0.advance_with(|input, path| {
            input.control.check()?;
            let state = std::mem::replace(&mut input.state, State::Failed);
            input.state = match state {
                State::Native(mut import) => {
                    if import.as_mut().advance_with_window(maximum_object_bytes)?
                        == SQLiteBackupImportStep::Complete
                    {
                        State::Imported
                    } else {
                        State::Native(import)
                    }
                }
                State::KeyValue(mut import) => {
                    if import.as_mut().advance_with_window(maximum_object_bytes)?
                        == SQLiteBackupImportStep::Complete
                    {
                        State::Imported
                    } else {
                        State::KeyValue(import)
                    }
                }
                State::Recover(recovery) => {
                    match input.target.mapping {
                        SQLiteBackupMapping::Native => SQLiteNativeBackupImport::recover(
                            path,
                            recovery.root,
                            recovery.request,
                            recovery.options.clone(),
                            &input.control,
                        )?,
                        SQLiteBackupMapping::KeyValue => SQLiteKeyValueBackupImport::recover(
                            path,
                            recovery.root,
                            recovery.request,
                            recovery.options.clone(),
                            &input.control,
                        )?,
                    }
                    State::Imported
                }
                State::Imported => State::Imported,
                State::Validating(_) => {
                    return Err(BackupError::Invalid("logical image validation is active"))
                }
                State::Failed => return Err(BackupError::Finished),
            };
            if matches!(input.state, State::Imported) {
                result = BackupImageImportStep::Imported;
            }
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
        let step = self.0.advance_with(|input, path| {
            let state = std::mem::replace(&mut input.state, State::Failed);
            let mut validation = match state {
                State::Imported => {
                    let provider = input
                        .target
                        .open_file(path, input.request, &input.control)?;
                    validator.begin(provider, &input.control)?
                }
                State::Validating(validation) => validation,
                _ => return Err(BackupError::Invalid("SQLite image import is incomplete")),
            };
            match validation.advance(maximum_steps)? {
                BackupImageValidationStep::Pending => {
                    input.state = State::Validating(validation);
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

    #[rstest::rstest]
    fn sqlite_destination_retains_validation_until_completion_and_recovers_cancellation(
        #[values(SQLiteBackupMapping::Native, SQLiteBackupMapping::KeyValue)]
        mapping: SQLiteBackupMapping,
        #[values(0, 1, 2, 3)] mode: usize,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let connection = ManagedConnection::open(&directory.path().join("source")).unwrap();
        let provider: Arc<dyn PersistentStorageProvider> = match mapping {
            SQLiteBackupMapping::Native => Arc::new(SQLiteStorageProvider::new(connection)),
            SQLiteBackupMapping::KeyValue => {
                Arc::new(SQLiteKeyValueStorage::from_connection(connection).unwrap())
            }
        };
        let mut target = SQLiteBackupTarget::new(mapping);
        target.compression = (mode >= 2).then(SQLiteCompressionOptions::default);
        target.encryption_key =
            (mode % 2 == 1).then(|| StorageEncryptionKey::new("validation fixture"));
        let target: Arc<dyn PersistentBackupTarget> = Arc::new(target);
        uqa_storage::key_value::conformance::verify_backup_validation_continuation(
            provider,
            &target,
            directory.path(),
        )
        .unwrap();
    }

    #[test]
    fn sqlite_restore_targets_never_create_a_missing_completed_database() {
        for mapping in [SQLiteBackupMapping::Native, SQLiteBackupMapping::KeyValue] {
            for mode in 0..4 {
                let directory = tempfile::tempdir().unwrap();
                let control = StorageReadControl::with_limit(1 << 20);
                let request =
                    DatabaseRestore::new(uqa_storage::mvcc::DatabaseId::from_bytes([31; 16]))
                        .unwrap();
                let mut target = SQLiteBackupTarget::new(mapping);
                target.compression = (mode >= 2).then(SQLiteCompressionOptions::default);
                target.encryption_key =
                    (mode % 2 == 1).then(|| StorageEncryptionKey::new("test secret"));
                assert!(target.open(directory.path(), request, &control).is_err());
                assert!(!directory.path().join(DATABASE).exists());
                assert_eq!(control.memory().used(), 0);
            }
        }
    }
}
