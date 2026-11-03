//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A private native image becomes usable only after archive and physical projection verification.

use super::{
    super::{format, projection::image::ImageProjection},
    SQLiteNativeImageImportReader,
};
use crate::{
    connection::image_import::ImageConnection,
    mvcc::{
        backup_import::{insert_identifier, insert_record, physical_error, sqlite_error},
        schema,
    },
    SQLiteBackupImportOptions, SQLiteBackupImportStep,
};
use std::path::Path;
use uqa_core::memory::MemoryReservation;
use uqa_storage::{
    backup::{
        BackupError, BackupLease, BackupResult, BackupRoot, BackupSourceStep, ImageProvider,
        RecordImageImportUnit,
    },
    mvcc::{BackupCursorKey, DatabaseId, DatabaseRestore},
    read_control::StorageReadControl,
};

/// Restore an image into an already created, empty private file. Raw values stream through the public incremental BLOB API. Projection and comparison retain one charged row at a time; the original memory allowance must cover that row and its bindings. An interrupted import remains unavailable. The enclosing restore owner still validates the logical catalog and artifacts before publishing its directory.
pub struct SQLiteNativeBackupImport<'a> {
    connection: Option<ImageConnection>,
    state: State<'a>,
    root: BackupRoot,
    request: DatabaseRestore,
    control: StorageReadControl,
    maximum_value_bytes: usize,
    records: u64,
    identifiers: u64,
}

enum State<'a> {
    Reading {
        input: Box<SQLiteNativeImageImportReader<'a>>,
        _memory: MemoryReservation,
        namespace: Option<DatabaseId>,
    },
    Projecting {
        projection: Box<ImageProjection>,
        _memory: MemoryReservation,
        namespace: DatabaseId,
    },
    Publishing {
        namespace: DatabaseId,
    },
    Complete,
    Failed,
}

impl<'a> SQLiteNativeBackupImport<'a> {
    pub fn new(
        path: &Path,
        lease: &'a BackupLease<'_>,
        request: DatabaseRestore,
        options: SQLiteBackupImportOptions,
        control: &StorageReadControl,
    ) -> BackupResult<Self> {
        control.check()?;
        let root = lease.root();
        if options.maximum_value_bytes == 0
            || request.source() != root.cut().source
            || request.target().as_bytes() == [0; 16]
        {
            return Err(BackupError::Invalid(
                "native image import options or identity",
            ));
        }
        let memory = control
            .memory()
            .reserve(std::mem::size_of::<SQLiteNativeImageImportReader<'_>>())?;
        let input =
            SQLiteNativeImageImportReader::new(lease, options.maximum_workspace_bytes, control)?;
        let connection =
            ImageConnection::create(path, options.compression, options.encryption_key, control)
                .map_err(sqlite_error)?;
        let physical = connection.physical().map_err(sqlite_error)?;
        schema::backup_import::initialize(physical, root, request, control)
            .map_err(physical_error)?;
        Ok(Self {
            connection: Some(connection),
            state: State::Reading {
                input: Box::new(input),
                _memory: memory,
                namespace: None,
            },
            root,
            request,
            control: control.clone(),
            maximum_value_bytes: options.maximum_value_bytes,
            records: 0,
            identifiers: 0,
        })
    }

    /// Recover a verified image's uncertain history publication without replaying rows or overwriting later commits. The exact archive and target binding is checked before restoration changes any state.
    pub fn recover(
        path: &Path,
        root: BackupRoot,
        request: DatabaseRestore,
        options: SQLiteBackupImportOptions,
        control: &StorageReadControl,
    ) -> BackupResult<()> {
        if root.cut().format.provider() != ImageProvider::SQLiteNative {
            return Err(BackupError::Invalid("native image recovery mapping"));
        }
        crate::mvcc::backup_import::recover_image(path, root, request, options, control)
    }

    pub const fn imported_records(&self) -> u64 {
        self.records
    }
    pub const fn imported_identifiers(&self) -> u64 {
        self.identifiers
    }

    /// Admit one input record, one physical row, or one fixed schema/verification boundary. Complete remains stable after later cancellation; every failed operation releases its input, row workspace and exclusive file owner.
    pub fn advance(&mut self) -> BackupResult<SQLiteBackupImportStep> {
        let result = self.advance_inner();
        if result.is_err() {
            self.state = State::Failed;
            self.connection = None;
        }
        result
    }

    fn advance_inner(&mut self) -> BackupResult<SQLiteBackupImportStep> {
        if matches!(self.state, State::Complete) {
            return Ok(SQLiteBackupImportStep::Complete);
        }
        if matches!(self.state, State::Failed) {
            return Err(BackupError::Finished);
        }
        self.control.check()?;
        let connection = self
            .connection
            .as_ref()
            .ok_or(BackupError::Finished)?
            .physical()
            .map_err(sqlite_error)?;
        match &mut self.state {
            State::Reading {
                input, namespace, ..
            } => {
                let step = input.advance(16 * 1024)?;
                if let Some(admitted) = input.native_namespace() {
                    *namespace = Some(admitted);
                }
                match step {
                    BackupSourceStep::Pending => (),
                    BackupSourceStep::Complete => {
                        let namespace = namespace
                            .ok_or(BackupError::Invalid("missing native image namespace"))?;
                        format::image::initialize(connection, namespace, &self.control)
                            .map_err(physical_error)?;
                        let memory = self
                            .control
                            .memory()
                            .reserve(std::mem::size_of::<ImageProjection>())?;
                        self.state = State::Projecting {
                            projection: Box::new(ImageProjection::new(
                                namespace,
                                self.root.cut().sequence.unwrap(),
                            )),
                            _memory: memory,
                            namespace,
                        };
                    }
                    BackupSourceStep::Unit => {
                        if import_unit(connection, input, self.maximum_value_bytes, &self.control)?
                        {
                            self.records += 1;
                        } else {
                            self.identifiers += 1;
                        }
                    }
                }
            }
            State::Projecting {
                projection,
                namespace,
                ..
            } => {
                if projection
                    .advance(connection, &self.control)
                    .map_err(physical_error)?
                {
                    self.state = State::Publishing {
                        namespace: *namespace,
                    };
                }
            }
            State::Publishing { namespace } => {
                format::image::complete(connection, *namespace, &self.control)
                    .map_err(physical_error)?;
                schema::backup_import::complete(connection, self.root, self.request, &self.control)
                    .map_err(physical_error)?;
                self.connection
                    .take()
                    .expect("active image owner")
                    .publish_history(self.root, self.request, &self.control)
                    .map_err(sqlite_error)?;
                self.state = State::Complete;
                return Ok(SQLiteBackupImportStep::Complete);
            }
            State::Complete | State::Failed => unreachable!(),
        }
        Ok(SQLiteBackupImportStep::Pending)
    }
}

fn import_unit(
    connection: &rusqlite::Connection,
    input: &mut SQLiteNativeImageImportReader<'_>,
    maximum_value_bytes: usize,
    control: &StorageReadControl,
) -> BackupResult<bool> {
    match input.current_unit().expect("admitted image unit") {
        RecordImageImportUnit::Identifier {
            namespace,
            watermark,
        } => {
            insert_identifier(connection, namespace, watermark, control)?;
            Ok(false)
        }
        RecordImageImportUnit::Record {
            key,
            revision,
            value_bytes,
        } => {
            let limit = maximum_value_bytes.min(i32::MAX as usize) as u64;
            if value_bytes > limit {
                return Err(BackupError::Limit {
                    resource: "native image row",
                    required: value_bytes,
                    limit,
                });
            }
            let mut owned = BackupCursorKey::new(control.memory());
            owned.extend_from_slice(key)?;
            insert_record(
                connection,
                &owned,
                revision.as_u64(),
                value_bytes as i32,
                |output| input.read_current(output),
                control,
            )?;
            Ok(true)
        }
    }
}

#[cfg(test)]
mod tests;
