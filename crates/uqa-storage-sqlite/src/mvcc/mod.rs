//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `SQLite` persistence for shared logical records, with short physical transactions and durable receipts.

mod codec;
mod key_value;
pub mod native;
mod read;
mod schema;
#[cfg(test)]
mod tests;
mod write;

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
/// All versions and receipts are currently retained. The Key/Value provider uses these records directly. `Self::for_native` converts a native catalog and atomically maintains its current rows with their history; native document sessions can bind through `ManagedConnection::bind_native_records`, while complete catalog/backend routing is still required before Engine can use that format.
#[derive(Clone)]
pub struct SQLiteRecordStore {
    connection: ManagedConnection,
    identity: DatabaseId,
    native: bool,
}

impl SQLiteRecordStore {
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

    /// Atomically import an initialized native schema 48 catalog, or reopen its versioned materialization. Unbound native store handles are disabled after conversion; callers must bind a routed native session or prepare native records through the shared transaction contract. This lower persistence adapter does not enable concurrent Engine SQL.
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

    fn check_transaction(&self, transaction: StorageTransactionId) -> VersionResult<()> {
        if transaction.database() != self.identity {
            return Err(VersionError::WrongDatabase);
        }
        Ok(())
    }
}

impl VersionedPersistence for SQLiteRecordStore {
    fn database_id(&self) -> DatabaseId {
        self.identity
    }
    fn graph_record_layout(&self) -> Option<&dyn uqa_storage::mvcc::GraphRecordLayout> {
        if self.native {
            None
        } else {
            Some(&uqa_storage::key_value::KeyValueGraphRecords)
        }
    }
    fn allocate_transaction(
        &self,
        control: &StorageReadControl,
    ) -> VersionResult<StorageTransactionId> {
        control.cancellation().check()?;
        self.with(|connection| write::allocate(connection, self.identity, self.native, control))
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
        self.with(|connection| {
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
