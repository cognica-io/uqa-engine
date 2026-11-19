//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Publish a legacy catalog's semantic restoration and native record baseline in one physical transaction.

use uqa_storage::mvcc::{VersionError, VersionedSessionOptions};
use uqa_storage::read_control::StorageReadControl;
use uqa_storage::KeyValueStore;

use super::logical::BoundRecordSession;
use super::{Arc, Connection, ManagedConnection, Result, SQLiteError, VersionedKeyValueStore};

pub(super) struct NativeRestore {
    _permit: crate::mvcc::WritePermit,
    _admission: Option<super::ownership::RecordAdmission>,
}

impl ManagedConnection {
    /// The initial native transition publishes an independent committed-image owner to its provider. The weak publication target cannot retain the provider or form a session cycle.
    pub(crate) fn new_native_initial_session(
        &self,
        source: std::sync::Weak<std::sync::OnceLock<Self>>,
    ) -> Self {
        let _gate = self.session.gate.read();
        let mut session = super::SessionState::new();
        session.native_image_source = Some(source);
        if let Some(logical) = self.session.logical.get() {
            let _ = session.logical.set(Arc::new(
                logical.new_session_with_cancellation(&session.write_cancellation),
            ));
        }
        let connection = Self {
            pool: Arc::clone(&self.pool),
            session: Arc::new(session),
            record_access: self.record_access,
        };
        if let Some(logical) = connection.session.logical.get() {
            connection.publish_native_image_source(logical);
        }
        connection
    }

    fn publish_native_image_source(&self, logical: &BoundRecordSession) {
        let Some(source) = self
            .session
            .native_image_source
            .as_ref()
            .and_then(std::sync::Weak::upgrade)
        else {
            return;
        };
        source.get_or_init(|| {
            let session = super::SessionState::new();
            let _ = session.logical.set(Arc::new(
                logical.new_session_with_cancellation(&session.write_cancellation),
            ));
            Self {
                pool: Arc::clone(&self.pool),
                session: Arc::new(session),
                record_access: self.record_access,
            }
        });
    }

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
        let admission = self.admit_record_initialization(&StorageReadControl::new(
            &uqa_core::memory::MemoryBudget::new(VersionedSessionOptions::default().retained_bytes),
            &self.write_cancellation(),
        ))?;
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
        *self.session.native_restore.lock() = Some(NativeRestore {
            _permit: permit,
            _admission: admission,
        });
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
        let namespace = records
            .native_namespace()
            .ok_or(SQLiteError::SessionMappingMismatch)?;
        Ok(Arc::new(BoundRecordSession {
            store: Arc::new(VersionedKeyValueStore::new_with_cancellation(
                Arc::new(records),
                identity,
                options,
                self.write_cancellation(),
            )),
            native: Some(namespace),
        }))
    }

    pub(super) fn install_initial_native_binding(&self, logical: Arc<BoundRecordSession>) {
        // The caller holds the session write gate, and all fallible preparation precedes physical COMMIT.
        assert!(
            self.session.logical.set(logical).is_ok(),
            "initial native session was already bound"
        );
        self.publish_native_image_source(
            self.session
                .logical
                .get()
                .expect("installed native binding"),
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
    fn native_initial_commit_publishes_an_independent_image_owner() {
        for mode in 0..4 {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("source");
            let base = match mode {
                0 => ManagedConnection::open(&path),
                1 => ManagedConnection::open_encrypted(&path, "image-owner"),
                2 => ManagedConnection::open_compressed(
                    &path,
                    crate::SQLiteCompressionOptions::default(),
                ),
                _ => ManagedConnection::open_compressed_encrypted(
                    &path,
                    "image-owner",
                    crate::SQLiteCompressionOptions::default(),
                ),
            }
            .unwrap();
            let source = Arc::new(std::sync::OnceLock::new());
            let weak = Arc::downgrade(&source);
            let initial = base.new_native_initial_session(weak.clone());
            initial.begin_native_initial_restore().unwrap();
            let catalog = Catalog::open(initial.clone()).unwrap();
            catalog.set_metadata("image", "rolled back").unwrap();
            initial.rollback_transaction().unwrap();
            assert!(source.get().is_none());
            drop(catalog);
            initial.begin_native_initial_restore().unwrap();
            let catalog = Catalog::open(initial.clone()).unwrap();
            catalog.set_metadata("image", "committed").unwrap();
            initial.commit_transaction().unwrap();
            assert!(
                !base.is_native_record_session(),
                "source publication does not bind the unrelated factory session"
            );
            let published = source.get().unwrap();
            assert!(published.is_native_record_session());
            initial.begin_transaction().unwrap();
            catalog.set_metadata("image", "private").unwrap();
            initial.write_cancellation().cancel();
            let source_catalog = Catalog::open(published.clone()).unwrap();
            assert_eq!(
                source_catalog.get_metadata("image").unwrap().as_deref(),
                Some("committed")
            );
            initial.rollback_transaction().unwrap();
            drop((source_catalog, catalog, initial, base));
            let control = StorageReadControl::with_limit(1 << 20);
            let image = crate::SQLiteNativeBackupImage::capture(published, &control).unwrap();
            drop(source);
            assert!(
                weak.upgrade().is_none(),
                "published image owners do not retain their publication cell"
            );
            drop(image);
            assert_eq!(control.memory().used(), 0);
        }
    }

    #[test]
    fn failed_native_initial_conversion_does_not_publish_an_image_owner() {
        let base = ManagedConnection::open_in_memory().unwrap();
        let source = Arc::new(std::sync::OnceLock::new());
        let initial = base.new_native_initial_session(Arc::downgrade(&source));
        initial.begin_native_initial_restore().unwrap();
        let catalog = Catalog::open(initial.clone()).unwrap();
        catalog.set_metadata("initial", "private").unwrap();
        initial.with(|sqlite| {
            sqlite.execute_batch("CREATE TRIGGER reject_baseline BEFORE UPDATE ON _metadata WHEN NEW.key = 'schema_version' AND NEW.value = '49' BEGIN SELECT RAISE(ABORT, 'injected baseline failure'); END")?;
            Ok(())
        }).unwrap();
        assert!(initial.commit_transaction().is_err());
        assert!(source.get().is_none());
        initial.rollback_transaction().unwrap();
        assert!(source.get().is_none());
        assert!(!base.is_native_record_session());
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
