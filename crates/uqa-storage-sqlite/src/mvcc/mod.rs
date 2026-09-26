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
#[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
pub(crate) mod leases;
pub mod native;
mod read;
pub(crate) mod receipts;
mod reclamation;
mod resources;
pub(crate) mod restore;
mod retention;
mod runs;
mod schema;
mod serializable;
#[cfg(test)]
mod tests;
mod tombstones;
mod write;

pub(crate) use schema::WritePermit;
pub use serializable::SQLiteSerializableAdmission;

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
/// Logical snapshot leases protect predecessor histories across bounded reads and native processes; reclamation preserves head tombstones and all commit receipts. The Key/Value provider uses these records directly. `Self::for_native` converts a native catalog and atomically maintains its current rows with their history; `ManagedConnection::bind_native_records` binds catalog and data handles to the same logical session.
#[derive(Clone)]
pub struct SQLiteRecordStore {
    connection: ManagedConnection,
    identity: DatabaseId,
    native: Option<native::NativeRecordNamespace>,
    snapshots: Arc<uqa_storage::mvcc::SnapshotRegistry>,
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
        let native::NativeMapping {
            identity,
            namespace,
        } = native::initialize_in(transaction, control).map_err(Error::into_version)?;
        Ok(Self {
            snapshots: retention::registry(connection, identity)?,
            connection: connection.record_connection(),
            identity,
            native: Some(namespace),
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
            snapshots: retention::registry(&connection, identity)?,
            connection,
            identity,
            native: None,
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
        let native::NativeMapping {
            identity,
            namespace,
        } = connection
            .with(|connection| Ok(native::initialize(connection, control)))
            .map_err(|error| VersionError::Storage(error.into()))?
            .map_err(Error::into_version)?;
        Ok(Self {
            snapshots: retention::registry(&connection, identity)?,
            connection,
            identity,
            native: Some(namespace),
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
            snapshots: retention::registry(&connection, identity)?,
            connection,
            identity,
            native: None,
        })
    }

    /// Immutable namespace for database-owned native record keys. Use it with `NativeRecordOwner::Database`; `VersionedPersistence::database_id` identifies the transaction history and may differ after restoration. Key/Value and raw record stores return `None`.
    #[must_use]
    pub fn native_namespace(&self) -> Option<DatabaseId> {
        self.native.map(|namespace| namespace.0)
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
    fn resource_leases(&self) -> Option<&dyn uqa_storage::mvcc::ResourceLeaseProvider> {
        Some(self)
    }

    fn serializable_coordinator(&self) -> Option<&dyn uqa_storage::mvcc::SerializableCoordinator> {
        Some(self)
    }

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
        match self.native.as_ref() {
            Some(namespace) => Some(namespace),
            None => Some(&uqa_storage::key_value::KeyValueGraphRecords),
        }
    }
    fn occurrence_record_layout(&self) -> &dyn uqa_storage::mvcc::OccurrenceRecordLayout {
        if self.native.is_some() {
            &crate::inverted_index::NativeOccurrenceRecords
        } else {
            &uqa_storage::key_value::KeyValueOccurrenceRecords
        }
    }
    fn notification_record_layout(&self) -> &dyn uqa_storage::mvcc::NotificationRecordLayout {
        match self.native.as_ref() {
            Some(namespace) => namespace,
            None => &uqa_storage::key_value::KeyValueNotificationRecords,
        }
    }

    fn maintenance_record_layout(&self) -> &dyn uqa_storage::mvcc::MaintenanceRecordLayout {
        if self.native.is_some() {
            &native::NativeMaintenanceRecords
        } else {
            &uqa_storage::key_value::KeyValueMaintenanceRecords
        }
    }
    fn ivf_record_layout(&self) -> &dyn uqa_storage::mvcc::IVFRecordLayout {
        if self.native.is_some() {
            &crate::vector_index::NativeIVFRecords
        } else {
            &uqa_storage::key_value::KeyValueIVFRecords
        }
    }
    fn hnsw_record_layout(&self) -> Option<&dyn uqa_storage::mvcc::HNSWRecordLayout> {
        if self.native.is_some() {
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

    fn allocate_managed_transaction(
        &self,
        control: &StorageReadControl,
    ) -> VersionResult<uqa_storage::mvcc::RetainedTransactionAllocation> {
        self.allocate_receipt_owner(control)
    }
    fn snapshot(
        &self,
        control: &StorageReadControl,
    ) -> VersionResult<Arc<dyn CommittedRecordSnapshot>> {
        control.cancellation().check()?;
        let mut reclamation_epoch = 0;
        let lease = self.snapshots.capture(control, || {
            self.with(|connection| {
                let read = connection.unchecked_transaction()?;
                native::check_mapping(&read, self.native)?;
                let sequence = codec::header(&read, self.identity)?.sequence;
                reclamation_epoch = tombstones::epoch(&read)?;
                read.commit()?;
                Ok(sequence)
            })
        })?;
        uqa_storage::mvcc::retain_record_snapshot(
            read::Snapshot {
                store: self.clone(),
                sequence: lease.sequence(),
                reclamation_epoch,
                _lease: lease,
            },
            control,
        )
    }
    fn reclaim_versions(&self, control: &StorageReadControl) -> VersionResult<u64> {
        self.snapshots.reclaim(control, |oldest| {
            self.with_write(control, |connection| {
                reclamation::reclaim(connection, self.identity, self.native, oldest, control)
            })
        })
    }

    fn reclaim_tombstones(
        &self,
        request: &uqa_storage::mvcc::TombstoneReclamationRequest<'_>,
        control: &StorageReadControl,
    ) -> VersionResult<uqa_storage::mvcc::TombstoneReclamationStep> {
        request.validate(control)?;
        self.snapshots.reclaim(control, |oldest| {
            if oldest.is_some() {
                return Ok(uqa_storage::mvcc::TombstoneReclamationStep::Retained);
            }
            self.with_write(control, |connection| {
                tombstones::reclaim(connection, self.identity, self.native, request, control)
            })
        })
    }

    fn reclaim_diskann_tombstones(&self, control: &StorageReadControl) -> VersionResult<()> {
        if self.native.is_none() {
            return uqa_storage::mvcc::reclaim_key_value_diskann_tombstones(self, control);
        }
        for family in [
            native::NativeRecordFamily::DiskANNRecords,
            native::NativeRecordFamily::VectorOrigins,
            native::NativeRecordFamily::VectorChanges,
        ] {
            let prefix = native::NativeRecordIdentity::family_prefix(family, control)?;
            uqa_storage::mvcc::reclaim_tombstone_prefix(self, &prefix, control)?;
        }
        Ok(())
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

    fn acknowledge_transaction(
        &self,
        acknowledgement: uqa_storage::mvcc::ReceiptAcknowledgement,
        control: &StorageReadControl,
    ) -> VersionResult<()> {
        self.check_transaction(acknowledgement.transaction())?;
        self.with_write(control, |connection| {
            receipts::acknowledge(connection, self.native, acknowledgement, control)
        })
    }

    fn reclaim_transaction_receipts(&self, control: &StorageReadControl) -> VersionResult<u64> {
        self.reclaim_receipts(control)
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
