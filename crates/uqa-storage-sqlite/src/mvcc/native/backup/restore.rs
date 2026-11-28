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
        backup_import::{physical_error, sqlite_error, ImageWriter},
        schema,
    },
    SQLiteBackupImportOptions, SQLiteBackupImportStep,
};
use std::path::Path;
use uqa_core::memory::MemoryReservation;
use uqa_storage::{
    backup::{
        BackupError, BackupLeaseInput, BackupRecordImageWriter, BackupResult, BackupRoot,
        BackupSourceStep, ImageProvider, RecordImageImportProgress,
    },
    key_value::KeyValueImageWriter,
    mvcc::{DatabaseId, DatabaseRestore},
    read_control::StorageReadControl,
};

/// Restore an image into an already created, empty private file. Raw values stream through the public incremental BLOB API. Projection and comparison retain one charged row at a time; the original memory allowance must cover that row and its bindings. An interrupted import remains unavailable. The enclosing restore owner still validates the logical catalog and artifacts before publishing its directory.
pub struct SQLiteNativeBackupImport<'a> {
    connection: Option<ImageWriter>,
    state: State<'a>,
    root: BackupRoot,
    request: DatabaseRestore,
    control: StorageReadControl,
    maximum_value_bytes: usize,
    progress: RecordImageImportProgress,
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
        lease: impl Into<BackupLeaseInput<'a>>,
        request: DatabaseRestore,
        options: SQLiteBackupImportOptions,
        control: &StorageReadControl,
    ) -> BackupResult<Self> {
        control.check()?;
        let lease = lease.into();
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
            connection: Some(ImageWriter::new(connection)),
            state: State::Reading {
                input: Box::new(input),
                _memory: memory,
                namespace: None,
            },
            root,
            request,
            control: control.clone(),
            maximum_value_bytes: options.maximum_value_bytes,
            progress: RecordImageImportProgress::default(),
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
        self.progress.records()
    }
    pub const fn imported_identifiers(&self) -> u64 {
        self.progress.identifiers()
    }

    /// Admit one input record, one physical row, or one fixed schema/verification boundary. Complete remains stable after later cancellation; every failed operation releases its input, row workspace and exclusive file owner.
    pub fn advance(&mut self) -> BackupResult<SQLiteBackupImportStep> {
        self.advance_with_window(16 * 1024)
    }

    /// Bound the whole-object verification pass by the supplied window. Record, projection and value limits apply independently, and a zero window retires active input.
    pub fn advance_with_window(
        &mut self,
        maximum_object_bytes: usize,
    ) -> BackupResult<SQLiteBackupImportStep> {
        let result = self.advance_inner(maximum_object_bytes);
        if result.is_err() {
            self.state = State::Failed;
            self.connection = None;
        }
        result
    }

    fn advance_inner(
        &mut self,
        maximum_object_bytes: usize,
    ) -> BackupResult<SQLiteBackupImportStep> {
        if matches!(self.state, State::Complete) {
            return Ok(SQLiteBackupImportStep::Complete);
        }
        if matches!(self.state, State::Failed) {
            return Err(BackupError::Finished);
        }
        if maximum_object_bytes == 0 {
            return Err(BackupError::Invalid("zero object verification window"));
        }
        self.control.check()?;
        let connection = self.connection.as_mut().ok_or(BackupError::Finished)?;
        connection.physical().map_err(sqlite_error)?;
        match &mut self.state {
            State::Reading {
                input, namespace, ..
            } => {
                let step = input.advance(maximum_object_bytes)?;
                if let Some(admitted) = input.native_namespace() {
                    *namespace = Some(admitted);
                }
                match step {
                    BackupSourceStep::Pending => (),
                    BackupSourceStep::Complete => {
                        let namespace = namespace
                            .ok_or(BackupError::Invalid("missing native image namespace"))?;
                        connection.flush(&self.control)?;
                        format::image::initialize(
                            connection.physical().map_err(sqlite_error)?,
                            namespace,
                            &self.control,
                        )
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
                        input.write_current(
                            &mut self.progress,
                            connection,
                            self.maximum_value_bytes,
                            &self.control,
                        )?;
                    }
                }
            }
            State::Projecting {
                projection,
                namespace,
                ..
            } => {
                if projection
                    .advance(connection.physical().map_err(sqlite_error)?, &self.control)
                    .map_err(physical_error)?
                {
                    self.state = State::Publishing {
                        namespace: *namespace,
                    };
                }
            }
            State::Publishing { namespace } => {
                format::image::complete(
                    connection.physical().map_err(sqlite_error)?,
                    *namespace,
                    &self.control,
                )
                .map_err(physical_error)?;
                self.connection
                    .take()
                    .expect("active image owner")
                    .activate(self.root, self.request, &self.control)?;
                self.state = State::Complete;
                return Ok(SQLiteBackupImportStep::Complete);
            }
            State::Complete | State::Failed => unreachable!(),
        }
        Ok(SQLiteBackupImportStep::Pending)
    }
}

#[cfg(test)]
mod tests;
