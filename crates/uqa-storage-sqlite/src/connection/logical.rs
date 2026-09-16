//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Connection clones and store handles share one logical transaction context.

use uqa_storage::mvcc::{VersionError, VersionedSessionOptions};
use uqa_storage::read_control::StorageReadControl;
use uqa_storage::{PersistentStorageIdentity, StorageBackendResult};

use super::{
    Arc, Connection, KeyValueStore, ManagedConnection, Result, SQLiteError, SessionState,
    VersionedKeyValueStore,
};

impl ManagedConnection {
    pub(crate) fn record_connection(&self) -> Self {
        Self {
            pool: Arc::clone(&self.pool),
            session: Arc::new(SessionState::new()),
            record_access: true,
        }
    }

    pub(crate) fn bind_records(
        &self,
        options: VersionedSessionOptions,
    ) -> Result<Arc<VersionedKeyValueStore>> {
        self.surface_cleanup_failure()?;
        let _gate = self.session.gate.write();
        if let Some(logical) = self.session.logical.get() {
            if logical.options().retained_bytes != options.retained_bytes {
                return Err(SQLiteError::SessionOptionsMismatch);
            }
            return Ok(Arc::clone(logical));
        }
        if self.session.transaction.lock().is_some() {
            return Err(SQLiteError::TransactionAlreadyActive);
        }
        let identity = self
            .database_path()
            .map(PersistentStorageIdentity::for_database_path)
            .transpose()?;
        let records = crate::SQLiteRecordStore::for_key_value(
            self,
            &StorageReadControl::with_limit(options.retained_bytes),
        )
        .map_err(VersionError::into_storage_error)?;
        let logical = Arc::new(VersionedKeyValueStore::new(
            Arc::new(records),
            identity,
            options,
        ));
        self.session
            .logical
            .set(Arc::clone(&logical))
            .map_err(|_| SQLiteError::SessionOptionsMismatch)?;
        Ok(logical)
    }

    pub(crate) fn with_records<R>(
        &self,
        operation: impl FnOnce(&VersionedKeyValueStore) -> StorageBackendResult<R>,
    ) -> StorageBackendResult<R> {
        self.surface_cleanup_failure()?;
        let _gate = self.session.gate.read();
        let logical = self
            .session
            .logical
            .get()
            .ok_or(SQLiteError::LogicalSessionRequired)?;
        operation(logical)
    }

    pub(crate) fn begin_record_read(&self) -> Result<()> {
        self.surface_cleanup_failure()?;
        let _gate = self.session.gate.write();
        let logical = self
            .session
            .logical
            .get()
            .ok_or(SQLiteError::LogicalSessionRequired)?;
        if logical.in_transaction() {
            return Err(SQLiteError::TransactionAlreadyActive);
        }
        logical.begin_read_transaction().map_err(Into::into)
    }

    pub(super) fn check_native_access(&self, connection: &Connection) -> Result<()> {
        if self.record_access {
            return Ok(());
        }
        let versioned: bool = connection.prepare_cached(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name = '_key_value' AND type = 'view')",
        )?.query_row([], |row| row.get(0))?;
        if versioned {
            return Err(SQLiteError::LogicalSessionRequired);
        }
        Ok(())
    }

    /// Access physical `SQLite` state for explicit diagnostics or maintenance outside any session transaction. This does not read private logical changes; ordinary stores must use their owning transaction interface. Record-table write guards remain active.
    pub fn with_physical<R>(&self, operation: impl FnOnce(&Connection) -> Result<R>) -> Result<R> {
        self.surface_cleanup_failure()?;
        let _gate = self.session.gate.write();
        if self.session.transaction.lock().is_some()
            || self
                .session
                .logical
                .get()
                .is_some_and(|logical| logical.in_transaction())
        {
            return Err(SQLiteError::TransactionAlreadyActive);
        }
        let connection = self.pool.checkout()?;
        operation(connection.connection()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uqa_storage::mvcc::{CommitFailure, DatabaseId, StorageTransactionId};
    use uqa_storage::StorageBackendError;

    #[test]
    fn storage_error_conversion_preserves_an_indeterminate_commit_identity() {
        let transaction = StorageTransactionId::new(DatabaseId::from_bytes([7; 16]), 3).unwrap();
        let original = StorageBackendError::backend(
            "MVCC",
            CommitFailure::Indeterminate {
                transaction,
                source: StorageBackendError::Other("commit reply lost".into()),
            },
        );
        let converted = StorageBackendError::from(SQLiteError::from(original));
        let StorageBackendError::Backend { backend, source } = converted else {
            panic!("typed commit outcome was lost");
        };
        assert_eq!(backend, "MVCC");
        assert!(
            matches!(source.downcast_ref::<CommitFailure>(), Some(CommitFailure::Indeterminate { transaction: found, .. }) if *found == transaction)
        );
    }
}
