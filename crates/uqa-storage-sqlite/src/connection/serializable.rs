//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain an independently scoped SSI connection with the main database's encryption policy.

use std::time::Duration;

use uqa_storage::{
    mvcc::{
        DatabaseId, SerializablePredicate, SerializableReadContext, SerializableSession,
        SerializableSnapshotCapture, SerializableSnapshotOptions, VersionError, VersionResult,
    },
    read_control::StorageReadControl,
    PersistentStorageIdentity, StorageBackendResult,
};

use super::ManagedConnection;
use crate::SQLiteConnectionLease;

impl SerializableSession for ManagedConnection {
    fn establish_serializable_snapshot(&self) -> StorageBackendResult<SerializableReadContext> {
        self.with_serializable_session(|session| session.establish_serializable_snapshot())
    }

    fn establish_serializable_snapshot_with(
        &self,
        options: SerializableSnapshotOptions,
        capture: &mut SerializableSnapshotCapture<'_>,
    ) -> StorageBackendResult<SerializableReadContext> {
        self.with_serializable_session(|session| {
            session.establish_serializable_snapshot_with(options, capture)
        })
    }

    fn serializable_read_context(&self) -> StorageBackendResult<Option<SerializableReadContext>> {
        self.with_serializable_session(|session| session.serializable_read_context())
    }

    fn observe_serializable_write(
        &self,
        predicate: SerializablePredicate<'_>,
    ) -> StorageBackendResult<()> {
        self.with_serializable_session(|session| session.observe_serializable_write(predicate))
    }
}

impl ManagedConnection {
    pub(crate) fn with_local_receipt_admission<T>(
        &self,
        state: Option<&std::sync::Arc<uqa_storage::mvcc::LocalSerializableState>>,
        control: &StorageReadControl,
        operation: impl FnOnce(&dyn uqa_storage::mvcc::SerializableLeases) -> VersionResult<T>,
    ) -> VersionResult<T> {
        state
            .unwrap_or(&self.pool.receipt_state)
            .with_admission(&self.pool, control, operation)
    }

    fn with_serializable_session<T>(
        &self,
        operation: impl FnOnce(&dyn SerializableSession) -> StorageBackendResult<T>,
    ) -> StorageBackendResult<T> {
        self.surface_cleanup_failure()?;
        let _gate = self.session.gate.read();
        let logical = self
            .session
            .logical
            .get()
            .ok_or(crate::SQLiteError::LogicalSessionRequired)?;
        operation(logical.store.as_ref())
    }

    pub(crate) fn serializable_local_leases(
        &self,
        control: &StorageReadControl,
    ) -> std::sync::Arc<uqa_storage::mvcc::LocalSerializableLeases> {
        let mut retained = self.pool.serializable_leases.lock();
        std::sync::Arc::clone(retained.get_or_insert_with(|| {
            std::sync::Arc::new(uqa_storage::mvcc::LocalSerializableLeases::new(
                control.memory(),
            ))
        }))
    }

    pub(crate) fn serializable_connection(
        &self,
        database: DatabaseId,
        control: &StorageReadControl,
    ) -> VersionResult<Self> {
        let mut retained = loop {
            control.check()?;
            if let Some(retained) = self
                .pool
                .serializable_connection
                .try_lock_for(Duration::from_millis(2))
            {
                break retained;
            }
        };
        if let Some((identity, connection)) = &*retained {
            if *identity != database {
                return Err(VersionError::WrongDatabase);
            }
            return Ok(connection.clone());
        }
        let connection = self
            .open_serializable_connection()
            .map_err(|error| VersionError::Storage(error.into()))?;
        *retained = Some((database, connection.clone()));
        Ok(connection)
    }

    /// Open the physical SSI database without attaching an incarnation-specific cache entry to the main pool.
    pub(crate) fn open_serializable_connection(&self) -> crate::Result<Self> {
        if let Some(path) = self.database_path() {
            let PersistentStorageIdentity::File(path) =
                PersistentStorageIdentity::for_database_path(path)?
            else {
                unreachable!("database file identity")
            };
            let mut auxiliary = path.into_os_string();
            auxiliary.push(".uqa-serializable");
            Self::from_spec_owned(
                super::ConnectionSpec::Auxiliary {
                    path: std::path::PathBuf::from(auxiliary),
                    key: self.auxiliary_encryption_key(),
                },
                super::default_pool_connections(),
                self.database_owner(),
            )
        } else {
            // The parent pool retains this one physical in-memory database even between record handles.
            Self::open_in_memory()
        }
    }

    pub(crate) fn lease_connection_with_control(
        &self,
        control: &StorageReadControl,
    ) -> crate::Result<SQLiteConnectionLease> {
        self.pool
            .checkout_with_cancellation(Some(control.cancellation()))
            .map(SQLiteConnectionLease)
    }
}
