//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Connection clones and store handles share one logical transaction context.

use uqa_storage::mvcc::{DatabaseId, VersionError, VersionedSessionOptions};
use uqa_storage::read_control::StorageReadControl;
use uqa_storage::{PersistentStorageIdentity, StorageBackendResult};

use super::{
    Arc, Connection, KeyValueStore, ManagedConnection, Result, SQLiteError, SessionState,
    VersionedKeyValueStore,
};

pub(super) struct BoundRecordSession {
    pub(super) store: Arc<VersionedKeyValueStore>,
    pub(super) native: Option<DatabaseId>,
}

impl std::ops::Deref for BoundRecordSession {
    type Target = VersionedKeyValueStore;

    fn deref(&self) -> &Self::Target {
        &self.store
    }
}

impl BoundRecordSession {
    pub(super) fn new_session_with_cancellation(
        &self,
        cancellation: &uqa_core::CancellationToken,
    ) -> Self {
        Self {
            store: Arc::new(self.store.new_session_with_cancellation(cancellation)),
            native: self.native,
        }
    }
}

impl ManagedConnection {
    pub(crate) fn snapshot_registry(
        &self,
        identity: DatabaseId,
    ) -> uqa_storage::mvcc::VersionResult<Arc<uqa_storage::mvcc::SnapshotRegistry>> {
        let mut retained = self.pool.snapshot_registry.lock();
        if let Some((old_identity, registry)) = retained.as_ref() {
            if let Some(registry) = registry.upgrade() {
                if *old_identity != identity {
                    return Err(VersionError::WrongDatabase);
                }
                return Ok(registry);
            }
        }
        let registry = Arc::new(uqa_storage::mvcc::SnapshotRegistry::default());
        *retained = Some((identity, Arc::downgrade(&registry)));
        Ok(registry)
    }

    /// Create an independent logical session over the same database pool.
    /// Explicit transactions started on either session are isolated and never
    /// capture operations issued through the other session.
    #[must_use]
    pub fn new_session(&self) -> Self {
        self.new_session_with_cancellation(&uqa_core::CancellationToken::new())
    }

    /// Use a fresh transaction context while sharing the invoking execution's write cancellation. Existing sessions and clones keep their original token.
    #[must_use]
    pub fn new_session_with_cancellation(
        &self,
        cancellation: &uqa_core::CancellationToken,
    ) -> Self {
        let _gate = self.session.gate.read();
        let session = SessionState::with_cancellation(cancellation.clone());
        if let Some(logical) = self.session.logical.get() {
            let _ = session.logical.set(Arc::new(
                logical.new_session_with_cancellation(cancellation),
            ));
        }
        Self {
            pool: Arc::clone(&self.pool),
            session: Arc::new(session),
            record_access: self.record_access,
        }
    }

    /// Keep the bound record view and its logical reader attribution in an independent read-only connection. The source session retains ownership of publication and transaction completion.
    pub(crate) fn new_retained_read_session(
        &self,
        cancellation: &uqa_core::CancellationToken,
    ) -> Result<Self> {
        self.surface_cleanup_failure()?;
        let _gate = self.session.gate.read();
        let logical = self
            .session
            .logical
            .get()
            .ok_or(SQLiteError::LogicalSessionRequired)?;
        let store = logical.new_retained_read_session(cancellation)?;
        let session = SessionState::with_cancellation(cancellation.clone());
        let _ = session.logical.set(Arc::new(BoundRecordSession {
            store: Arc::new(store),
            native: logical.native,
        }));
        Ok(Self {
            pool: Arc::clone(&self.pool),
            session: Arc::new(session),
            record_access: self.record_access,
        })
    }

    /// Share the token used for autonomous allocation and record publication. Cleanup reads and rollback remain independent of this flag.
    pub fn write_cancellation(&self) -> uqa_core::CancellationToken {
        self.session.write_cancellation.clone()
    }

    /// Transaction ownership selected for this connection and inherited by its independent sessions.
    pub fn transaction_model(&self) -> uqa_storage::StorageTransactionModel {
        let _gate = self.session.gate.read();
        self.session.logical.get().map_or(
            uqa_storage::StorageTransactionModel::ProviderSerialized,
            |logical| logical.transaction_model(),
        )
    }

    pub(crate) fn record_connection(&self) -> Self {
        Self {
            pool: Arc::clone(&self.pool),
            session: Arc::new(SessionState::new()),
            record_access: true,
        }
    }

    pub(crate) fn with_record_connection<R>(
        &self,
        control: &StorageReadControl,
        operation: impl FnOnce(&Connection) -> Result<R>,
    ) -> Result<R> {
        self.surface_cleanup_failure()?;
        let _gate = self.session.gate.read();
        if !self.record_access || self.session.transaction.lock().is_some() {
            return Err(SQLiteError::SessionMappingMismatch);
        }
        let connection = self
            .pool
            .checkout_with_cancellation(Some(control.cancellation()))?;
        operation(connection.connection()?)
    }

    pub(crate) fn bind_records(
        &self,
        options: VersionedSessionOptions,
    ) -> Result<Arc<VersionedKeyValueStore>> {
        self.surface_cleanup_failure()?;
        let _gate = self.session.gate.write();
        if let Some(logical) = self.session.logical.get() {
            if logical.native.is_some() {
                return Err(SQLiteError::SessionMappingMismatch);
            }
            if logical.options().retained_bytes != options.retained_bytes {
                return Err(SQLiteError::SessionOptionsMismatch);
            }
            return Ok(Arc::clone(&logical.store));
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
        let logical = Arc::new(VersionedKeyValueStore::new_with_cancellation(
            Arc::new(records),
            identity,
            options,
            self.write_cancellation(),
        ));
        self.session
            .logical
            .set(Arc::new(BoundRecordSession {
                store: Arc::clone(&logical),
                native: None,
            }))
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
        if logical.native.is_some() {
            return Err(SQLiteError::SessionMappingMismatch.into());
        }
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

    /// Advance a bound native or Key/Value command view through the common session, without opening a physical writer. Legacy physical transactions require their existing transaction boundary instead.
    pub fn refresh_transaction_snapshot(
        &self,
        cancellation: &uqa_core::CancellationToken,
    ) -> Result<()> {
        self.surface_cleanup_failure()?;
        let _gate = self.session.gate.write();
        self.session
            .logical
            .get()
            .ok_or(SQLiteError::LogicalSessionRequired)?
            .refresh_transaction_snapshot(cancellation)
            .map_err(Into::into)
    }

    pub(super) fn check_native_access(&self, connection: &Connection) -> Result<()> {
        if self.record_access {
            return Ok(());
        }
        let versioned: bool = connection.prepare_cached(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE (name = '_key_value' AND type = 'view') OR name GLOB '_uqa_mvcc_native_*')",
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
    use uqa_storage::mvcc::{CommitErrorOutcome, CommitFailure, DatabaseId, StorageTransactionId};
    use uqa_storage::StorageBackendError;

    #[test]
    fn record_checkout_cancellation_does_not_wait_for_a_retained_pool_connection() {
        use std::{sync::mpsc, thread, time::Duration};

        let connection = ManagedConnection::open_in_memory()
            .unwrap()
            .record_connection();
        let held = connection.pool.checkout().unwrap();
        let control = StorageReadControl::with_limit(1 << 20);
        let cancellation = control.cancellation().clone();
        let (started_tx, started_rx) = mpsc::channel();
        let (finished_tx, finished_rx) = mpsc::channel();
        thread::scope(|scope| {
            let worker = scope.spawn(|| {
                started_tx.send(()).unwrap();
                let result = connection.with_record_connection(&control, |_| {
                    panic!("cancelled checkout must not enter the operation")
                });
                finished_tx.send(result).unwrap();
            });
            started_rx.recv_timeout(Duration::from_secs(30)).unwrap();
            cancellation.cancel();
            let result = finished_rx.recv_timeout(Duration::from_secs(30));
            drop(held);
            worker.join().unwrap();
            assert!(matches!(
                result.unwrap(),
                Err::<(), _>(SQLiteError::Cancelled(_))
            ));
        });
    }

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
        assert_eq!(
            converted.commit_outcome(),
            Some(CommitErrorOutcome::Indeterminate(transaction))
        );
        let StorageBackendError::Backend { backend, source } = converted else {
            panic!("typed commit outcome was lost");
        };
        assert_eq!(backend, "MVCC");
        assert!(
            matches!(source.downcast_ref::<CommitFailure>(), Some(CommitFailure::Indeterminate { transaction: found, .. }) if *found == transaction)
        );
    }
}
