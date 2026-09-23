//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Pure-Rust redb implementation of UQA's ordered Key/Value storage contract.
//!
//! [`RedbStorage`] owns one database and implements
//! [`uqa_storage::PersistentStorageProvider`]. Every opened engine session gets
//! an independent [`RedbKeyValueStore`] transaction state while sharing the
//! same MVCC database.

mod error;
mod mvcc;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use redb::Database;
use uqa_storage::{
    CatalogFacade, KeyValueCatalog, KeyValueStorageBackend, KeyValueStore,
    PersistentStorageBackend, PersistentStorageIdentity, PersistentStorageProvider,
    PersistentStorageSession, StorageBackendError, StorageBackendResult,
};

pub use mvcc::RedbRecordStore;
pub use uqa_storage::mvcc::{VersionedKeyValueStore as RedbKeyValueStore, VersionedSessionOptions};

use error::redb_error;
use uqa_storage::read_control::{CancellationToken, StorageReadControl};

/// Shared redb database owner and engine-session factory.
#[derive(Clone)]
pub struct RedbStorage {
    records: Arc<RedbRecordStore>,
    identity: PathBuf,
    options: VersionedSessionOptions,
}

impl RedbStorage {
    /// Open an existing redb database or create a new one at `path`.
    pub fn open(path: impl AsRef<Path>) -> StorageBackendResult<Self> {
        Self::open_with_options(path, VersionedSessionOptions::default())
    }

    /// Open with an explicit per-session retention limit. Private changes are held in bounded memory; no plaintext spill files are created.
    pub fn open_with_options(
        path: impl AsRef<Path>,
        options: VersionedSessionOptions,
    ) -> StorageBackendResult<Self> {
        let path = path.as_ref();
        let database = Arc::new(Database::create(path).map_err(redb_error)?);
        let records = RedbRecordStore::new(database)
            .map_err(uqa_storage::mvcc::VersionError::into_storage_error)?;
        Self::from_records(path, records, options)
    }

    /// Open a closed, consistent backup as a new database history. Every prior provider, session, snapshot and serializable participant for `path` must be closed; redb's exclusive file ownership enforces this before any restore mutation.
    ///
    /// Retain `request` outside the database before calling. Retry that same request after an error because the durable transition may have completed. A retry after completion preserves new receipts and writes. A separate restoration requires a new request. Ordinary reopen uses `open` or `open_with_options` and preserves the existing incarnation and outcomes.
    pub fn open_restored(
        path: impl AsRef<Path>,
        request: uqa_storage::mvcc::DatabaseRestore,
        options: VersionedSessionOptions,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        let path = path.as_ref();
        let database = Database::open(path).map_err(redb_error)?;
        let records = mvcc::restore::open(database, request, control)
            .map_err(uqa_storage::mvcc::VersionError::into_storage_error)?;
        Self::from_records(path, records, options)
    }

    fn from_records(
        path: &Path,
        records: RedbRecordStore,
        options: VersionedSessionOptions,
    ) -> StorageBackendResult<Self> {
        records
            .migrate_key_value()
            .map_err(uqa_storage::mvcc::VersionError::into_storage_error)?;
        let identity = std::fs::canonicalize(path).map_err(|error| {
            StorageBackendError::Other(format!(
                "canonicalize redb database `{}`: {error}",
                path.display()
            ))
        })?;
        Ok(Self {
            records: Arc::new(records),
            identity,
            options,
        })
    }

    /// Create an independent logical session without acquiring a physical writer.
    pub fn store(&self) -> RedbKeyValueStore {
        self.store_with_cancellation(&CancellationToken::new())
    }

    fn store_with_cancellation(&self, cancellation: &CancellationToken) -> RedbKeyValueStore {
        RedbKeyValueStore::new_with_cancellation(
            self.records.clone(),
            Some(PersistentStorageIdentity::File(self.identity.clone())),
            self.options,
            cancellation.clone(),
        )
    }

    /// Share the record persistence used by this owner's Key/Value and catalog sessions.
    pub fn record_store(&self) -> uqa_storage::mvcc::VersionResult<RedbRecordStore> {
        Ok((*self.records).clone())
    }
}

impl PersistentStorageProvider for RedbStorage {
    fn open_session(&self) -> StorageBackendResult<PersistentStorageSession> {
        self.open_session_with_cancellation(&CancellationToken::new())
    }

    fn open_session_with_cancellation(
        &self,
        cancellation: &CancellationToken,
    ) -> StorageBackendResult<PersistentStorageSession> {
        cancellation.check()?;
        let store: Arc<dyn KeyValueStore> = Arc::new(self.store_with_cancellation(cancellation));
        let catalog: Arc<dyn CatalogFacade> = Arc::new(KeyValueCatalog::new(Arc::clone(&store)));
        let backend: Arc<dyn PersistentStorageBackend> =
            Arc::new(KeyValueStorageBackend::new(store));
        Ok(PersistentStorageSession::new(catalog, backend))
    }

    fn storage_identity(&self) -> StorageBackendResult<Option<PersistentStorageIdentity>> {
        Ok(Some(PersistentStorageIdentity::File(self.identity.clone())))
    }
}
