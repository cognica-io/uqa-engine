//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Publish a legacy catalog's semantic restoration and native record baseline in one physical transaction.

use uqa_storage::mvcc::{VersionError, VersionedPersistence, VersionedSessionOptions};
use uqa_storage::read_control::StorageReadControl;
use uqa_storage::KeyValueStore;

use super::logical::BoundRecordSession;
use super::{Arc, Connection, ManagedConnection, Result, SQLiteError, VersionedKeyValueStore};

pub(super) struct NativeRestore {
    _permit: crate::mvcc::WritePermit,
}

impl ManagedConnection {
    pub(crate) fn begin_native_initial_restore(&self) -> Result<()> {
        self.surface_cleanup_failure()?;
        let _gate = self.session.gate.write();
        if let Some(logical) = self.session.logical.get() {
            return logical.begin_transaction().map_err(Into::into);
        }
        let mut transaction = self.session.transaction.lock();
        if transaction.is_some() {
            return Err(SQLiteError::TransactionAlreadyActive);
        }
        let connection = self.pool.checkout()?;
        let sqlite = connection.connection()?;
        let permit = crate::mvcc::WritePermit::for_native_restore(sqlite)
            .map_err(VersionError::into_storage_error)?;
        sqlite.execute_batch("BEGIN IMMEDIATE")?;
        if crate::SQLiteRecordStore::has_native_mapping(sqlite)
            .map_err(VersionError::into_storage_error)?
        {
            // Inspect under the physical reservation: another opener may have converted the file after this session was created.
            let logical = self.prepare_initial_native_binding(sqlite)?;
            sqlite.execute_batch("COMMIT")?;
            self.install_initial_native_binding(Arc::clone(&logical));
            drop(permit);
            drop(connection);
            return logical.begin_transaction().map_err(Into::into);
        }
        self.session.transaction_failure.lock().take();
        *self.session.native_restore.lock() = Some(NativeRestore { _permit: permit });
        *transaction = Some(connection);
        Ok(())
    }

    pub(super) fn prepare_initial_native_binding(
        &self,
        transaction: &Connection,
    ) -> Result<Arc<BoundRecordSession>> {
        let identity = self.storage_identity()?;
        let options = VersionedSessionOptions::default();
        let records = crate::SQLiteRecordStore::initialize_native_in(
            self,
            transaction,
            &StorageReadControl::with_limit(options.retained_bytes),
        )
        .map_err(VersionError::into_storage_error)?;
        let database = records.database_id();
        Ok(Arc::new(BoundRecordSession {
            store: Arc::new(VersionedKeyValueStore::new_with_cancellation(
                Arc::new(records),
                identity,
                options,
                self.write_cancellation(),
            )),
            native: Some(database),
        }))
    }

    pub(super) fn install_initial_native_binding(&self, logical: Arc<BoundRecordSession>) {
        // The caller holds the session write gate, and all fallible preparation precedes physical COMMIT.
        assert!(
            self.session.logical.set(logical).is_ok(),
            "initial native session was already bound"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Catalog;

    #[test]
    fn rollback_preserves_the_legacy_catalog_and_its_unbound_handles() {
        let connection = ManagedConnection::open_in_memory().unwrap();
        let catalog = Catalog::open(connection.clone()).unwrap();
        catalog.set_metadata("original", "committed").unwrap();
        connection.begin_native_initial_restore().unwrap();
        catalog.set_metadata("original", "private").unwrap();
        connection.rollback_transaction().unwrap();
        assert!(!connection.transaction_model().is_versioned());
        assert_eq!(
            catalog.get_metadata("original").unwrap().as_deref(),
            Some("committed")
        );
        connection
            .with(|sqlite| {
                assert!(!crate::SQLiteRecordStore::has_native_mapping(sqlite).unwrap());
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn conversion_failure_rolls_back_catalog_bootstrap_and_closes_write_admission() {
        let connection = ManagedConnection::open_in_memory().unwrap();
        connection.begin_native_initial_restore().unwrap();
        let catalog = Catalog::open(connection.clone()).unwrap();
        catalog.set_metadata("initial", "private").unwrap();
        connection.with(|sqlite| {
            sqlite.execute_batch("CREATE TRIGGER reject_baseline BEFORE UPDATE ON _metadata WHEN NEW.key = 'schema_version' AND NEW.value = '49' BEGIN SELECT RAISE(ABORT, 'injected baseline failure'); END")?;
            Ok(())
        }).unwrap();
        let error = connection.commit_transaction().unwrap_err();
        assert!(
            error.to_string().contains("injected baseline failure"),
            "{error}"
        );
        connection.rollback_transaction().unwrap();
        connection
            .with(|sqlite| {
                assert_eq!(
                    sqlite.query_row(
                        "SELECT count(*) FROM sqlite_schema WHERE name NOT GLOB 'sqlite_*'",
                        [],
                        |row| row.get::<_, i64>(0)
                    )?,
                    0
                );
                assert_eq!(
                    sqlite.query_row("SELECT __uqa_mvcc_write_permit()", [], |row| row
                        .get::<_, i64>(0))?,
                    0
                );
                Ok(())
            })
            .unwrap();
        connection.begin_native_initial_restore().unwrap();
        Catalog::open(connection.clone()).unwrap();
        connection.commit_transaction().unwrap();
        assert!(connection.transaction_model().is_versioned());
        connection
            .with_physical(|sqlite| {
                assert!(sqlite
                    .execute("UPDATE _uqa_mvcc_metadata SET allocated = allocated", [])
                    .is_err());
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn initial_sessions_recheck_peer_conversion_and_attach_existing_catalog_handles() {
        let directory = tempfile::tempdir().unwrap();
        let connection = ManagedConnection::open(&directory.path().join("initial.db")).unwrap();
        let peer = connection.new_session();
        let first = Catalog::for_initial_restore(connection.clone());
        let second = Catalog::for_initial_restore(peer.clone());
        connection.begin_native_initial_restore().unwrap();
        Catalog::open(connection.clone()).unwrap();
        first.set_metadata("initial", "committed").unwrap();
        connection.commit_transaction().unwrap();
        peer.begin_native_initial_restore().unwrap();
        Catalog::open(peer.clone()).unwrap();
        assert!(peer.transaction_model().is_versioned());
        assert_eq!(
            second.get_metadata("initial").unwrap().as_deref(),
            Some("committed")
        );
        second.set_metadata("initial", "private").unwrap();
        assert_eq!(
            first.get_metadata("initial").unwrap().as_deref(),
            Some("committed")
        );
        peer.commit_transaction().unwrap();
        assert_eq!(
            first.get_metadata("initial").unwrap().as_deref(),
            Some("private")
        );
    }
}
