//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Verified Key/Value image import streams values through the existing physical BLOB owner.

mod writer;
pub(super) use writer::ImageWriter;

use std::path::Path;

use uqa_storage::{
    backup::{
        BackupError, BackupImageImportStep, BackupLeaseInput, BackupResult, BackupRoot,
        ImageProvider,
    },
    key_value::{KeyValueImageImportDriver, KeyValueImageImportReader},
    mvcc::DatabaseRestore,
    read_control::StorageReadControl,
    StorageEncryptionKey,
};

use super::schema;
use crate::{connection::image_import::ImageConnection, SQLiteCompressionOptions, SQLiteError};

/// Destination protection and per-record/repository workspace limits. The native `SQLite` page cache is separate from the original encoded-buffer allowance.
#[derive(Clone, Debug)]
pub struct SQLiteBackupImportOptions {
    pub compression: Option<SQLiteCompressionOptions>,
    pub encryption_key: Option<StorageEncryptionKey>,
    pub maximum_value_bytes: usize,
    pub maximum_workspace_bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SQLiteBackupImportStep {
    /// One image unit or one bounded repository verification window completed.
    Pending,
    /// All input is verified, the new transaction history is durable, and the physical owner is closed.
    Complete,
}

/// Import into an already created empty private file. The enclosing restore operation must retain the exact request durably, validate the complete logical catalog/artifacts, and publish the directory only after this owner closes. This primitive does not publish a destination directory or provide Engine jobs.
///
/// Each advance imports at most one record, writing its value through `SQLite`'s public incremental BLOB interface in at most 16 KiB pieces. The configured value bound limits one advance; a whole value is never copied into a UQA buffer. Interrupted input retains format zero and cannot open through either ordinary sessions or history restoration. After complete repository verification, the existing restore state machine replaces the main and auxiliary histories. Resolve an uncertain final outcome with [`Self::recover`], which checks the exact retained image and request before entering history recovery; never restart image import into a populated file.
pub struct SQLiteKeyValueBackupImport<'a>(KeyValueImageImportDriver<'a, ImageWriter>);

impl<'a> SQLiteKeyValueBackupImport<'a> {
    pub fn new(
        path: &Path,
        lease: impl Into<BackupLeaseInput<'a>>,
        request: DatabaseRestore,
        options: SQLiteBackupImportOptions,
        control: &StorageReadControl,
    ) -> BackupResult<Self> {
        control.cancellation().check()?;
        if options.maximum_value_bytes == 0 {
            return Err(BackupError::Invalid("zero SQLite import value limit"));
        }
        let lease = lease.into();
        let root = lease.root();
        if request.source() != root.cut().source || request.target().as_bytes() == [0; 16] {
            return Err(BackupError::Invalid("SQLite import restore identity"));
        }
        let input = KeyValueImageImportReader::new(
            lease,
            ImageProvider::SQLiteKV,
            options.maximum_workspace_bytes,
            control,
        )?;
        let connection =
            ImageConnection::create(path, options.compression, options.encryption_key, control)
                .map_err(sqlite_error)?;
        schema::backup_import::initialize(
            connection.physical().map_err(sqlite_error)?,
            root,
            request,
            control,
        )
        .map_err(physical_error)?;
        Ok(Self(KeyValueImageImportDriver::new(
            input,
            ImageWriter::new(connection),
            request,
            options.maximum_value_bytes.min(i32::MAX as usize),
            "SQLite import value",
            control,
        )))
    }

    /// Resolve a completely verified image's uncertain history transition without reading the archive again. The stored root and target must exactly match before the history owner admits recovery. Partial format-zero images remain unavailable. A completed transition preserves subsequent writes. Every physical connection closes before success; logical catalog and artifact validation still precede directory publication. Only protection options are used because recovery imports no values or repository workspace.
    pub fn recover(
        path: &Path,
        root: BackupRoot,
        request: DatabaseRestore,
        options: SQLiteBackupImportOptions,
        control: &StorageReadControl,
    ) -> BackupResult<()> {
        if root.cut().format.provider() != ImageProvider::SQLiteKV {
            return Err(BackupError::Invalid("Key/Value image recovery mapping"));
        }
        recover_image(path, root, request, options, control)
    }

    pub const fn imported_records(&self) -> u64 {
        self.0.imported_records()
    }
    pub const fn imported_identifiers(&self) -> u64 {
        self.0.imported_identifiers()
    }

    pub fn advance(&mut self) -> BackupResult<SQLiteBackupImportStep> {
        self.advance_with_window(16 * 1024)
    }

    /// Advance with a caller-selected whole-object verification window. At most one record is imported; its separate value limit and bounded BLOB writes remain unchanged.
    pub fn advance_with_window(
        &mut self,
        maximum_object_bytes: usize,
    ) -> BackupResult<SQLiteBackupImportStep> {
        Ok(match self.0.advance(maximum_object_bytes)? {
            BackupImageImportStep::Pending => SQLiteBackupImportStep::Pending,
            BackupImageImportStep::Imported => SQLiteBackupImportStep::Complete,
        })
    }
}

pub(super) fn sqlite_error(error: SQLiteError) -> BackupError {
    uqa_storage::StorageBackendError::from(error).into()
}
fn driver_error(error: rusqlite::Error) -> BackupError {
    sqlite_error(error.into())
}
pub(super) fn physical_error(error: super::Error) -> BackupError {
    error.into_version().into()
}

pub(super) fn recover_image(
    path: &Path,
    root: BackupRoot,
    request: DatabaseRestore,
    options: SQLiteBackupImportOptions,
    control: &StorageReadControl,
) -> BackupResult<()> {
    control.check()?;
    if request.source() != root.cut().source || request.target().as_bytes() == [0; 16] {
        return Err(BackupError::Invalid("SQLite import restore identity"));
    }
    let connection =
        ImageConnection::reopen(path, options.compression, options.encryption_key, control)
            .map_err(sqlite_error)?;
    connection
        .publish_history(root, request, control)
        .map_err(sqlite_error)
}
