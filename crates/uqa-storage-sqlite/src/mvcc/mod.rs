//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `SQLite` persistence for shared logical records, with short physical transactions and durable receipts.

mod codec;
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
/// All versions and receipts are currently retained. Existing native relational and Key/Value stores still need routing and format migration before they can use this transaction model.
#[derive(Clone)]
pub struct SQLiteRecordStore {
    connection: ManagedConnection,
    identity: DatabaseId,
}

impl SQLiteRecordStore {
    pub fn new(connection: &ManagedConnection) -> VersionResult<Self> {
        if connection.in_transaction() {
            return Err(VersionError::Storage(
                SQLiteError::TransactionAlreadyActive.into(),
            ));
        }
        let connection = connection.new_session();
        let identity = connection
            .with(|connection| Ok(schema::initialize(connection)))
            .map_err(|error| VersionError::Storage(error.into()))?
            .map_err(Error::into_version)?;
        Ok(Self {
            connection,
            identity,
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
    fn allocate_transaction(
        &self,
        control: &StorageReadControl,
    ) -> VersionResult<StorageTransactionId> {
        control.cancellation().check()?;
        self.with(|connection| write::allocate(connection, self.identity, control))
    }
    fn snapshot(
        &self,
        control: &StorageReadControl,
    ) -> VersionResult<Arc<dyn CommittedRecordSnapshot>> {
        control.cancellation().check()?;
        let sequence =
            self.with(|connection| Ok(codec::header(connection, self.identity)?.sequence))?;
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
        self.with(|connection| Ok(write::commit(connection, transaction, prepared, control)))?
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
        self.with(|connection| write::abort(connection, transaction, control))
    }
}
