//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Verified Key/Value image import streams values through the existing physical BLOB owner.

use std::{io::Write, path::Path};

use rusqlite::{params, Connection};
use uqa_storage::{
    backup::{BackupError, BackupLease, BackupResult, BackupRoot, ImageProvider},
    key_value::{KeyValueImageImportReader, KeyValueImageStep, KeyValueImportUnit},
    mvcc::{BackupCursorKey, DatabaseRestore},
    read_control::StorageReadControl,
    StorageEncryptionKey,
};

use super::{admission, schema};
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
pub struct SQLiteKeyValueBackupImport<'a> {
    connection: Option<ImageConnection>,
    input: Option<KeyValueImageImportReader<'a>>,
    root: BackupRoot,
    request: DatabaseRestore,
    control: StorageReadControl,
    maximum_value_bytes: usize,
    records: u64,
    identifiers: u64,
    complete: bool,
}

impl<'a> SQLiteKeyValueBackupImport<'a> {
    pub fn new(
        path: &Path,
        lease: &'a BackupLease<'_>,
        request: DatabaseRestore,
        options: SQLiteBackupImportOptions,
        control: &StorageReadControl,
    ) -> BackupResult<Self> {
        control.cancellation().check()?;
        if options.maximum_value_bytes == 0 {
            return Err(BackupError::Invalid("zero SQLite import value limit"));
        }
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
        Ok(Self {
            connection: Some(connection),
            input: Some(input),
            root,
            request,
            control: control.clone(),
            maximum_value_bytes: options.maximum_value_bytes,
            records: 0,
            identifiers: 0,
            complete: false,
        })
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
        self.records
    }
    pub const fn imported_identifiers(&self) -> u64 {
        self.identifiers
    }

    pub fn advance(&mut self) -> BackupResult<SQLiteBackupImportStep> {
        let result = self.advance_inner();
        if result.is_err() {
            self.input = None;
            self.connection = None;
        }
        result
    }

    fn advance_inner(&mut self) -> BackupResult<SQLiteBackupImportStep> {
        if self.complete {
            return Ok(SQLiteBackupImportStep::Complete);
        }
        let input = self.input.as_mut().ok_or(BackupError::Finished)?;
        match input.advance(16 * 1024)? {
            KeyValueImageStep::Pending => return Ok(SQLiteBackupImportStep::Pending),
            KeyValueImageStep::Complete => {
                let connection = self
                    .connection
                    .take()
                    .expect("active import owns its connection");
                schema::backup_import::complete(
                    connection.physical().map_err(sqlite_error)?,
                    self.root,
                    self.request,
                    &self.control,
                )
                .map_err(physical_error)?;
                connection
                    .publish_history(self.root, self.request, &self.control)
                    .map_err(sqlite_error)?;
                self.input = None;
                self.complete = true;
                return Ok(SQLiteBackupImportStep::Complete);
            }
            KeyValueImageStep::Unit => (),
        }
        let connection = self
            .connection
            .as_ref()
            .expect("active import owns its connection")
            .physical()
            .map_err(sqlite_error)?;
        match input.current_unit().expect("selected mapping unit") {
            KeyValueImportUnit::Identifier {
                namespace,
                watermark,
            } => {
                insert_identifier(connection, namespace, watermark, &self.control)?;
                self.identifiers += 1;
            }
            KeyValueImportUnit::Record {
                key,
                revision,
                value_bytes,
            } => {
                let limit = self.maximum_value_bytes.min(i32::MAX as usize) as u64;
                if value_bytes > limit {
                    return Err(BackupError::Limit {
                        resource: "SQLite import value",
                        required: value_bytes,
                        limit,
                    });
                }
                let mut owned_key = BackupCursorKey::new(self.control.memory());
                owned_key.extend_from_slice(key)?;
                insert_record(
                    connection,
                    &owned_key,
                    revision.as_u64(),
                    value_bytes as i32,
                    |output| input.read_current(output),
                    &self.control,
                )?;
                self.records += 1;
            }
        }
        Ok(SQLiteBackupImportStep::Pending)
    }
}

pub(super) fn insert_identifier(
    connection: &Connection,
    namespace: &[u8],
    watermark: u64,
    control: &StorageReadControl,
) -> BackupResult<()> {
    let _bindings =
        crate::read_control::reserve_bindings(control, &[namespace, &watermark.to_be_bytes()])
            .map_err(sqlite_error)?;
    let _timeout = admission::BusyTimeout::new(connection).map_err(driver_error)?;
    let _permit = admission::permit(connection, control).map_err(physical_error)?;
    let transaction = admission::begin(connection, control).map_err(physical_error)?;
    transaction
        .execute(
            "INSERT INTO _uqa_mvcc_identifiers (namespace, watermark) VALUES (?1, ?2)",
            params![namespace, watermark.to_be_bytes().as_slice()],
        )
        .map_err(driver_error)?;
    transaction.execute("INSERT INTO _uqa_mvcc_identifier_epochs VALUES (?1, x'0000000000000000', x'0000000000000000')", [namespace]).map_err(driver_error)?;
    admission::commit(transaction, control).map_err(physical_error)
}

pub(super) fn insert_record(
    connection: &Connection,
    key: &[u8],
    revision: u64,
    length: i32,
    mut read: impl FnMut(&mut [u8]) -> BackupResult<usize>,
    control: &StorageReadControl,
) -> BackupResult<()> {
    let _bindings = crate::read_control::reserve_bindings(control, &[key, &revision.to_be_bytes()])
        .map_err(sqlite_error)?;
    let _buffer_memory = control.memory().reserve(16 * 1024)?;
    let _timeout = admission::BusyTimeout::new(connection).map_err(driver_error)?;
    let _permit = admission::permit(connection, control).map_err(physical_error)?;
    let transaction = admission::begin(connection, control).map_err(physical_error)?;
    transaction
        .execute(
            "INSERT INTO _uqa_mvcc_versions (key, sequence, value) VALUES (?1, ?2, zeroblob(?3))",
            params![key, revision.to_be_bytes().as_slice(), length],
        )
        .map_err(driver_error)?;
    let row = transaction.last_insert_rowid();
    {
        let mut blob = transaction
            .blob_open("main", "_uqa_mvcc_versions", "value", row, false)
            .map_err(driver_error)?;
        let mut remaining = length as usize;
        let mut buffer = [0; 16 * 1024];
        while remaining != 0 {
            control.cancellation().check()?;
            let n = read(&mut buffer[..remaining.min(16 * 1024)])?;
            if n == 0 {
                return Err(BackupError::Invalid("truncated SQLite image value"));
            }
            blob.write_all(&buffer[..n])?;
            remaining -= n;
        }
        if read(&mut buffer[..1])? != 0 {
            return Err(BackupError::Invalid("oversized SQLite image value"));
        }
        blob.close().map_err(driver_error)?;
    }
    transaction
        .execute(
            "INSERT INTO _uqa_mvcc_heads (key, sequence) VALUES (?1, ?2)",
            params![key, revision.to_be_bytes().as_slice()],
        )
        .map_err(driver_error)?;
    admission::commit(transaction, control).map_err(physical_error)
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
