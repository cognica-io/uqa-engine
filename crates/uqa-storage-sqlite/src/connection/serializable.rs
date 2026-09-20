//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain an independently scoped SSI connection with the main database's encryption policy.

use std::time::Duration;

use uqa_storage::{
    mvcc::{DatabaseId, VersionError, VersionResult},
    read_control::StorageReadControl,
    PersistentStorageIdentity,
};

use super::ManagedConnection;
use crate::SQLiteConnectionLease;

impl ManagedConnection {
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
        let connection = if let Some(path) = self.database_path() {
            let PersistentStorageIdentity::File(path) =
                PersistentStorageIdentity::for_database_path(path)?
            else {
                unreachable!("database file identity")
            };
            let mut auxiliary = path.into_os_string();
            auxiliary.push(".uqa-serializable");
            Self::open_auxiliary(
                std::path::Path::new(&auxiliary),
                self.auxiliary_encryption_key(),
            )
        } else {
            // The parent pool retains this one physical in-memory database even between record handles.
            Self::open_in_memory()
        }
        .map_err(|error| VersionError::Storage(error.into()))?;
        *retained = Some((database, connection.clone()));
        Ok(connection)
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
