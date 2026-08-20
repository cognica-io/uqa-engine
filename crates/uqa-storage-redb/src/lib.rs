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

mod batch;
mod error;
mod store;
mod transaction;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use redb::Database;
use uqa_storage::{
    CatalogFacade, KeyValueCatalog, KeyValueStorageBackend, KeyValueStore,
    PersistentStorageBackend, PersistentStorageIdentity, PersistentStorageProvider,
    PersistentStorageSession, StorageBackendError, StorageBackendResult,
};

pub use store::RedbKeyValueStore;

use error::redb_error;
use store::initialize_database;

/// Shared redb database owner and engine-session factory.
#[derive(Clone)]
pub struct RedbStorage {
    database: Arc<Database>,
    identity: PathBuf,
}

impl RedbStorage {
    /// Open an existing redb database or create a new one at `path`.
    pub fn open(path: impl AsRef<Path>) -> StorageBackendResult<Self> {
        let path = path.as_ref();
        let database = Database::create(path).map_err(redb_error)?;
        initialize_database(&database)?;
        let identity = std::fs::canonicalize(path).map_err(|error| {
            StorageBackendError::Other(format!(
                "canonicalize redb database `{}`: {error}",
                path.display()
            ))
        })?;
        Ok(Self {
            database: Arc::new(database),
            identity,
        })
    }

    /// Create a transaction-isolated physical store session.
    pub fn store(&self) -> RedbKeyValueStore {
        RedbKeyValueStore::new(Arc::clone(&self.database), self.identity.clone())
    }
}

impl PersistentStorageProvider for RedbStorage {
    fn open_session(&self) -> StorageBackendResult<PersistentStorageSession> {
        let store: Arc<dyn KeyValueStore> = Arc::new(self.store());
        let catalog: Arc<dyn CatalogFacade> = Arc::new(KeyValueCatalog::new(Arc::clone(&store)));
        let backend: Arc<dyn PersistentStorageBackend> =
            Arc::new(KeyValueStorageBackend::new(store));
        Ok(PersistentStorageSession::new(catalog, backend))
    }

    fn storage_identity(&self) -> StorageBackendResult<Option<PersistentStorageIdentity>> {
        Ok(Some(PersistentStorageIdentity::File(self.identity.clone())))
    }
}
