//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `SQLite` persistence for shared logical records, with short physical transactions and durable receipts.

mod admission;
mod codec;
mod identifiers;
mod key_value;
pub mod native;
mod read;
mod schema;
#[cfg(test)]
mod tests;
mod write;

pub(crate) use schema::WritePermit;

use std::sync::Arc;

use rusqlite::Connection;
use uqa_storage::mvcc::{
    CommitResult, CommitStatus, CommittedRecordSnapshot, DatabaseId, PreparedRecordCommit,
    StorageTransactionId, VersionError, VersionResult, VersionedPersistence,
};
use uqa_storage::read_control::StorageReadControl;

use crate::{ManagedConnection, SQLiteError};

#[derive(Debug, thiserror::Error)]
enum Error {
    #[error(transparent)]
    SQLite(#[from] rusqlite::Error),
    #[error(transparent)]
    Version(#[from] VersionError),
}

impl Error {
    fn into_version(self) -> VersionError {
        match self {
            Self::SQLite(error) => sqlite_error(error),
            Self::Version(error) => error,
        }
    }
}

impl From<SQLiteError> for Error {
    fn from(error: SQLiteError) -> Self {
        match error {
            SQLiteError::Memory(error) => Self::Version(VersionError::Memory(error)),
            SQLiteError::Cancelled(error) => Self::Version(VersionError::Cancelled(error)),
            SQLiteError::SQLite(error) => Self::SQLite(error),
            error => Self::Version(VersionError::Storage(error.into())),
        }
    }
}

type PhysicalResult<T> = std::result::Result<T, Error>;

fn sqlite_error(error: rusqlite::Error) -> VersionError {
    VersionError::Storage(SQLiteError::SQLite(error).into())
}

/// Record persistence over a managed `SQLite` pool, including `SQLCipher` and compressed connections. Snapshots retain logical sequences; this adapter does not retain physical transactions between operations.
///
/// All versions and receipts are currently retained. The Key/Value provider uses these records directly. `Self::for_native` converts a native catalog and atomically maintains its current rows with their history; `ManagedConnection::bind_native_records` binds catalog and data handles to the same logical session.
#[derive(Clone)]
pub struct SQLiteRecordStore {
    connection: ManagedConnection,
    identity: DatabaseId,
    native: bool,
}

impl SQLiteRecordStore {
    pub(crate) fn has_native_mapping(connection: &Connection) -> VersionResult<bool> {
        native::present(connection).map_err(Error::into_version)
    }

    /// Prepare a native baseline on the initial restore's physical transaction. Its owner publishes both the restored catalog and this baseline with one COMMIT.
    pub(crate) fn initialize_native_in(
        connection: &ManagedConnection,
        transaction: &Connection,
        control: &StorageReadControl,
    ) -> VersionResult<Self> {
        let identity = native::initialize_in(transaction, control).map_err(Error::into_version)?;
        Ok(Self {
            connection: connection.record_connection(),
            identity,
            native: true,
        })
    }

    pub fn new(connection: &ManagedConnection) -> VersionResult<Self> {
        if connection.in_transaction() {
            return Err(VersionError::Storage(
                SQLiteError::TransactionAlreadyActive.into(),
            ));
        }
        let connection = connection.record_connection();
        let identity = connection
            .with(|connection| Ok(schema::initialize(connection)))
            .map_err(|error| VersionError::Storage(error.into()))?
            .map_err(Error::into_version)?;
        Ok(Self {
            connection,
            identity,
            native: false,
        })
    }

    /// Atomically initialize or upgrade a native catalog and import its baseline, or reopen its versioned materialization. Unbound native store handles are disabled after conversion; callers must bind a routed native session or prepare native records through the shared transaction contract.
    pub fn for_native(
        connection: &ManagedConnection,
        control: &StorageReadControl,
    ) -> VersionResult<Self> {
        if connection.in_transaction() {
            return Err(VersionError::Storage(
                SQLiteError::TransactionAlreadyActive.into(),
            ));
        }
        let connection = connection.record_connection();
        let identity = connection
            .with(|connection| Ok(native::initialize(connection, control)))
            .map_err(|error| VersionError::Storage(error.into()))?
            .map_err(Error::into_version)?;
        Ok(Self {
            connection,
            identity,
            native: true,
        })
    }

    pub(crate) fn for_key_value(
        connection: &ManagedConnection,
        control: &StorageReadControl,
    ) -> VersionResult<Self> {
        let connection = connection.record_connection();
        let identity = connection
            .with(|connection| Ok(key_value::initialize(connection, control)))
            .map_err(|error| VersionError::Storage(error.into()))?
            .map_err(Error::into_version)?;
        Ok(Self {
            connection,
            identity,
            native: false,
        })
    }

    fn with<T>(
        &self,
        operation: impl FnOnce(&Connection) -> PhysicalResult<T>,
    ) -> VersionResult<T> {
        self.connection
            .with(|connection| Ok(operation(connection)))
            .map_err(|error| VersionError::Storage(error.into()))?
            .map_err(Error::into_version)
    }

    fn with_write<T>(
        &self,
        control: &StorageReadControl,
        operation: impl FnOnce(&Connection) -> PhysicalResult<T>,
    ) -> VersionResult<T> {
        let mut operation = Some(operation);
        loop {
            control.cancellation().check()?;
            let result = self
                .connection
                .with_record_connection(control, |connection| {
                    let _timeout = admission::BusyTimeout::new(connection)?;
                    control.cancellation().check()?;
                    Ok(operation.take().expect("record write runs once")(
                        connection,
                    ))
                });
            match result {
                Err(SQLiteError::SQLite(error))
                    if operation.is_some() && admission::is_busy(&error) =>
                {
                    // Checkout/configuration failed before the operation was invoked.
                    admission::wait(control).map_err(Error::into_version)?;
                }
                result => {
                    return result
                        .map_err(|error| Error::from(error).into_version())?
                        .map_err(Error::into_version);
                }
            }
        }
    }

    fn check_transaction(&self, transaction: StorageTransactionId) -> VersionResult<()> {
        if transaction.database() != self.identity {
            return Err(VersionError::WrongDatabase);
        }
        Ok(())
    }
}

impl VersionedPersistence for SQLiteRecordStore {
    fn identifier_watermark(
        &self,
        namespace: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<Option<u64>> {
        control.cancellation().check()?;
        self.with(|connection| {
            identifiers::read(connection, self.identity, self.native, namespace, control)
        })
    }

    fn allocate_identifiers(
        &self,
        namespace: &[u8],
        request: uqa_storage::mvcc::IdentifierRequest,
        control: &StorageReadControl,
    ) -> VersionResult<uqa_storage::mvcc::IdentifierAllocation> {
        control.cancellation().check()?;
        self.with_write(control, |connection| {
            identifiers::allocate(
                connection,
                self.identity,
                self.native,
                namespace,
                request,
                control,
            )
        })
    }

    fn database_id(&self) -> DatabaseId {
        self.identity
    }
    fn graph_record_layout(&self) -> Option<&dyn uqa_storage::mvcc::GraphRecordLayout> {
        if self.native {
            Some(&native::NativeGraphRecords)
        } else {
            Some(&uqa_storage::key_value::KeyValueGraphRecords)
        }
    }
    fn occurrence_record_layout(&self) -> &dyn uqa_storage::mvcc::OccurrenceRecordLayout {
        if self.native {
            &crate::inverted_index::NativeOccurrenceRecords
        } else {
            &uqa_storage::key_value::KeyValueOccurrenceRecords
        }
    }
    fn maintenance_record_layout(&self) -> &dyn uqa_storage::mvcc::MaintenanceRecordLayout {
        if self.native {
            &native::NativeMaintenanceRecords
        } else {
            &uqa_storage::key_value::KeyValueMaintenanceRecords
        }
    }
    fn ivf_record_layout(&self) -> &dyn uqa_storage::mvcc::IVFRecordLayout {
        if self.native {
            &crate::vector_index::NativeIVFRecords
        } else {
            &uqa_storage::key_value::KeyValueIVFRecords
        }
    }
    fn hnsw_record_layout(&self) -> Option<&dyn uqa_storage::mvcc::HNSWRecordLayout> {
        if self.native {
            Some(&crate::vector_index::NativeHNSWRecords)
        } else {
            Some(&uqa_storage::key_value::KeyValueHNSWRecords)
        }
    }
    fn allocate_transaction(
        &self,
        control: &StorageReadControl,
    ) -> VersionResult<StorageTransactionId> {
        control.cancellation().check()?;
        self.with_write(control, |connection| {
            write::allocate(connection, self.identity, self.native, control)
        })
    }
    fn snapshot(
        &self,
        control: &StorageReadControl,
    ) -> VersionResult<Arc<dyn CommittedRecordSnapshot>> {
        control.cancellation().check()?;
        let sequence = self.with(|connection| {
            let read = connection.unchecked_transaction()?;
            native::check_mapping(&read, self.native)?;
            let sequence = codec::header(&read, self.identity)?.sequence;
            read.commit()?;
            Ok(sequence)
        })?;
        uqa_storage::mvcc::retain_record_snapshot(
            read::Snapshot {
                store: self.clone(),
                sequence,
            },
            control,
        )
    }
    fn commit(
        &self,
        transaction: StorageTransactionId,
        prepared: &PreparedRecordCommit,
        control: &StorageReadControl,
    ) -> CommitResult {
        self.check_transaction(transaction)?;
        control.cancellation().check().map_err(VersionError::from)?;
        let _bindings = write::reserve_bindings(prepared, control)?;
        self.with_write(control, |connection| {
            Ok(write::commit(
                connection,
                transaction,
                prepared,
                self.native,
                control,
            ))
        })?
    }
    fn commit_status(
        &self,
        transaction: StorageTransactionId,
        control: &StorageReadControl,
    ) -> VersionResult<CommitStatus> {
        self.check_transaction(transaction)?;
        control.cancellation().check()?;
        self.with(|connection| {
            let read = connection.unchecked_transaction()?;
            native::check_mapping(&read, self.native)?;
            codec::header(&read, self.identity)?;
            let status = codec::status(&read, transaction)?;
            control.cancellation().check().map_err(VersionError::from)?;
            read.commit()?;
            Ok(status)
        })
    }
    fn abort(
        &self,
        transaction: StorageTransactionId,
        control: &StorageReadControl,
    ) -> VersionResult<CommitStatus> {
        self.check_transaction(transaction)?;
        control.cancellation().check()?;
        self.with(|connection| write::abort(connection, transaction, self.native, control))
    }
}
