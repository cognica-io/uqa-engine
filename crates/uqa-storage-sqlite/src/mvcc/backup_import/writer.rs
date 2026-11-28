//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! One private `SQLite` write transaction spans bounded record and identifier inputs. The connection rolls it back on close.

use super::{driver_error, physical_error, sqlite_error, ImageConnection};
use crate::mvcc::{admission, schema};
use rusqlite::{params, Connection, DropBehavior};
use std::io::Write;
use uqa_storage::{
    backup::{BackupError, BackupRecordImageWriter, BackupResult, BackupRoot},
    key_value::KeyValueImageWriter,
    mvcc::{CommitSequence, DatabaseRestore},
    read_control::StorageReadControl,
};

pub(in crate::mvcc) struct ImageWriter {
    connection: ImageConnection,
    permit: Option<schema::WritePermit>,
}

impl ImageWriter {
    pub(in crate::mvcc) fn new(connection: ImageConnection) -> Self {
        Self {
            connection,
            permit: None,
        }
    }

    pub(in crate::mvcc) fn physical(&self) -> crate::Result<&Connection> {
        self.connection.physical()
    }

    fn transaction(&mut self, control: &StorageReadControl) -> BackupResult<&Connection> {
        let connection = self.connection.physical().map_err(sqlite_error)?;
        if self.permit.is_none() {
            let _timeout = admission::BusyTimeout::new(connection).map_err(driver_error)?;
            let permit = admission::permit(connection, control).map_err(physical_error)?;
            let mut transaction = admission::begin(connection, control).map_err(physical_error)?;
            // The native transaction lives on this exclusive connection across advances. On error or drop the connection closes before the permit is released.
            transaction.set_drop_behavior(DropBehavior::Ignore);
            self.permit = Some(permit);
        }
        Ok(connection)
    }
}

impl BackupRecordImageWriter for ImageWriter {
    fn identifier(
        &mut self,
        namespace: &[u8],
        watermark: u64,
        control: &StorageReadControl,
    ) -> BackupResult<()> {
        let _bindings =
            crate::read_control::reserve_bindings(control, &[namespace, &watermark.to_be_bytes()])
                .map_err(sqlite_error)?;
        let transaction = self.transaction(control)?;
        transaction
            .execute(
                "INSERT INTO _uqa_mvcc_identifiers (namespace, watermark) VALUES (?1, ?2)",
                params![namespace, watermark.to_be_bytes().as_slice()],
            )
            .map_err(driver_error)?;
        transaction.execute("INSERT INTO _uqa_mvcc_identifier_epochs VALUES (?1, x'0000000000000000', x'0000000000000000')", [namespace]).map_err(driver_error)?;
        Ok(())
    }

    fn record(
        &mut self,
        key: &[u8],
        revision: CommitSequence,
        length: usize,
        read: &mut dyn FnMut(&mut [u8]) -> BackupResult<usize>,
        control: &StorageReadControl,
    ) -> BackupResult<()> {
        let length =
            i32::try_from(length).map_err(|_| BackupError::Invalid("SQLite image value length"))?;
        let sequence = revision.as_u64();
        let _bindings =
            crate::read_control::reserve_bindings(control, &[key, &sequence.to_be_bytes()])
                .map_err(sqlite_error)?;
        let _buffer_memory = control.memory().reserve(16 * 1024)?;
        let transaction = self.transaction(control)?;
        transaction
        .execute(
            "INSERT INTO _uqa_mvcc_versions (key, sequence, value) VALUES (?1, ?2, zeroblob(?3))",
            params![key, sequence.to_be_bytes().as_slice(), length],
        )
        .map_err(driver_error)?;
        let row = transaction.last_insert_rowid();
        let mut hasher = uqa_storage::backup::RecordImageHasher::new(revision);
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
                hasher.update(&buffer[..n]);
                remaining -= n;
            }
            if read(&mut buffer[..1])? != 0 {
                return Err(BackupError::Invalid("oversized SQLite image value"));
            }
            blob.close().map_err(driver_error)?;
        }
        super::super::version_metadata::set_image_digest(
            transaction,
            key,
            revision,
            hasher.finish(),
        )
        .map_err(physical_error)?;
        transaction
            .execute(
                "INSERT INTO _uqa_mvcc_heads (key, sequence) VALUES (?1, ?2)",
                params![key, sequence.to_be_bytes().as_slice()],
            )
            .map_err(driver_error)?;
        Ok(())
    }

    fn flush(&mut self, control: &StorageReadControl) -> BackupResult<()> {
        if self.permit.is_some() {
            let connection = self.connection.physical().map_err(sqlite_error)?;
            let _timeout = admission::BusyTimeout::new(connection).map_err(driver_error)?;
            admission::commit_open(connection, control).map_err(physical_error)?;
            self.permit = None;
        }
        Ok(())
    }
}

impl KeyValueImageWriter for ImageWriter {
    fn activate(
        self,
        root: BackupRoot,
        request: DatabaseRestore,
        control: &StorageReadControl,
    ) -> BackupResult<()> {
        schema::backup_import::complete(
            self.physical().map_err(sqlite_error)?,
            root,
            request,
            control,
        )
        .map_err(physical_error)?;
        self.connection
            .publish_history(root, request, control)
            .map_err(sqlite_error)
    }
}
