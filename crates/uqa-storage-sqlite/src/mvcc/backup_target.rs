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
use std::{path::Path, sync::Arc};
use uqa_core::memory::Budgeted;
use uqa_storage::{
    backup::{
        restored_database_path, BackupError, BackupFileInput, BackupImageImportStep,
        BackupImportLimits, BackupLease, BackupResult, BackupRoot, ImageProvider,
        OwnedPersistentBackupImport, PersistentBackupTarget, RestoreDirectory,
    },
    mvcc::{DatabaseRestore, VersionedSessionOptions},
    read_control::StorageReadControl,
    PersistentStorageProvider, StorageEncryptionKey,
};

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
        let target = self.clone();
        OwnedPersistentBackupImport::from_file(
            directory,
            recover,
            control,
            |file, path| {
                let state = if file.is_none() {
                    let memory = control.memory().reserve(size_of::<Recovery>())?;
                    State::Recover(Budgeted::new(
                        Box::new(Recovery {
                            mapping: self.mapping,
                            root: backup.root(),
                            request,
                            options: self.options(limits),
                        }),
                        memory,
                    ))
                } else {
                    drop(file);
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
                Ok(state)
            },
            move |path, control| target.open_file(path, request, control),
        )
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
        let path = restored_database_path(directory, control)?;
        self.open_file(&path, request, control)
    }
}

struct Recovery {
    mapping: SQLiteBackupMapping,
    root: BackupRoot,
    request: DatabaseRestore,
    options: SQLiteBackupImportOptions,
}

enum State<'a> {
    Native(Budgeted<Box<SQLiteNativeBackupImport<'a>>>),
    KeyValue(Budgeted<Box<SQLiteKeyValueBackupImport<'a>>>),
    Recover(Budgeted<Box<Recovery>>),
}

impl BackupFileInput for State<'_> {
    fn advance(
        &mut self,
        path: &Path,
        maximum_object_bytes: usize,
        control: &StorageReadControl,
    ) -> BackupResult<BackupImageImportStep> {
        let step = match self {
            Self::Native(import) => import.as_mut().advance_with_window(maximum_object_bytes)?,
            Self::KeyValue(import) => import.as_mut().advance_with_window(maximum_object_bytes)?,
            Self::Recover(recovery) => {
                match recovery.mapping {
                    SQLiteBackupMapping::Native => SQLiteNativeBackupImport::recover(
                        path,
                        recovery.root,
                        recovery.request,
                        recovery.options.clone(),
                        control,
                    )?,
                    SQLiteBackupMapping::KeyValue => SQLiteKeyValueBackupImport::recover(
                        path,
                        recovery.root,
                        recovery.request,
                        recovery.options.clone(),
                        control,
                    )?,
                }
                SQLiteBackupImportStep::Complete
            }
        };
        Ok(match step {
            SQLiteBackupImportStep::Pending => BackupImageImportStep::Pending,
            SQLiteBackupImportStep::Complete => BackupImageImportStep::Imported,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[rstest::rstest]
    fn stored_view_queries_read_the_original_physical_cut(
        #[values(SQLiteBackupMapping::Native, SQLiteBackupMapping::KeyValue)]
        mapping: SQLiteBackupMapping,
    ) {
        verify_stored_catalog_input(
            mapping,
            uqa_storage::key_value::conformance::verify_backup_view_queries,
        );
    }

    #[rstest::rstest]
    fn stored_metadata_values_read_the_original_physical_cut(
        #[values(SQLiteBackupMapping::Native, SQLiteBackupMapping::KeyValue)]
        mapping: SQLiteBackupMapping,
    ) {
        verify_stored_catalog_input(
            mapping,
            uqa_storage::key_value::conformance::verify_backup_metadata_values,
        );
    }

    fn verify_stored_catalog_input(
        mapping: SQLiteBackupMapping,
        verify: fn(&dyn PersistentStorageProvider) -> BackupResult<()>,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let connection = ManagedConnection::open(&directory.path().join("catalog")).unwrap();
        let provider: Box<dyn PersistentStorageProvider> = match mapping {
            SQLiteBackupMapping::Native => Box::new(SQLiteStorageProvider::new(connection)),
            SQLiteBackupMapping::KeyValue => {
                Box::new(SQLiteKeyValueStorage::from_connection(connection).unwrap())
            }
        };
        verify(&*provider).unwrap();
    }

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
                assert!(!directory.path().join("database").exists());
                assert_eq!(control.memory().used(), 0);
            }
        }
    }
}
