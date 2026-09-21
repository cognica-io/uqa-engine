//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Database identity belongs to the physical pool, independently of its logical sessions.

use std::path::Path;
use uqa_storage::{PersistentStorageIdentity, StorageBackendError, StorageBackendResult};

use super::{ConnectionSpec, ManagedConnection};

impl ManagedConnection {
    /// Return the backing database path for a file-backed connection.
    #[must_use]
    pub fn database_path(&self) -> Option<&Path> {
        match &self.pool.spec {
            ConnectionSpec::File { path, .. }
            | ConnectionSpec::Auxiliary { path, .. }
            | ConnectionSpec::Compressed { path, .. } => Some(path),
            ConnectionSpec::Memory => None,
        }
    }

    pub(crate) fn storage_identity(
        &self,
    ) -> StorageBackendResult<Option<PersistentStorageIdentity>> {
        if let Some(path) = self.database_path() {
            return PersistentStorageIdentity::for_database_path(path).map(Some);
        }
        let mut identity = self.pool.memory_identity.lock();
        if identity.is_none() {
            let mut bytes = [0_u8; 16];
            getrandom::fill(&mut bytes).map_err(|error| {
                StorageBackendError::Other(format!(
                    "generate SQLite memory database identity: {error}"
                ))
            })?;
            *identity = Some(format!(
                "uqa-sqlite-memory:{:032x}",
                u128::from_be_bytes(bytes)
            ));
        }
        Ok(identity.clone().map(PersistentStorageIdentity::Opaque))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_database_identity_survives_sessions_and_distinguishes_physical_pools() {
        let connection = ManagedConnection::open_in_memory().unwrap();
        let identity = connection.storage_identity().unwrap();
        assert!(matches!(
            identity,
            Some(PersistentStorageIdentity::Opaque(_))
        ));
        assert_eq!(connection.clone().storage_identity().unwrap(), identity);
        assert_eq!(
            connection.new_session().storage_identity().unwrap(),
            identity
        );
        assert_eq!(
            connection.record_connection().storage_identity().unwrap(),
            identity
        );
        let sibling = connection.new_session();
        drop(connection);
        assert_eq!(sibling.storage_identity().unwrap(), identity);
        assert_ne!(
            ManagedConnection::open_in_memory()
                .unwrap()
                .storage_identity()
                .unwrap(),
            identity
        );
    }

    #[test]
    fn file_database_identity_is_shared_by_independent_pools() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("identity.db");
        let first = ManagedConnection::open(&path).unwrap();
        let second = ManagedConnection::open(&path).unwrap();
        let expected = Some(PersistentStorageIdentity::for_database_path(&path).unwrap());
        assert_eq!(first.storage_identity().unwrap(), expected);
        assert_eq!(second.storage_identity().unwrap(), expected);
    }

    #[test]
    fn native_and_key_value_factories_forward_their_memory_database_identity() {
        use std::sync::Arc;
        use uqa_storage::PersistentStorageProvider;

        for native in [false, true] {
            let connection = ManagedConnection::open_in_memory().unwrap();
            let identity = connection.storage_identity().unwrap();
            let provider: Arc<dyn PersistentStorageProvider> = if native {
                Arc::new(crate::SQLiteStorageProvider::new(connection))
            } else {
                Arc::new(crate::SQLiteKeyValueStorage::from_connection(connection).unwrap())
            };
            assert_eq!(provider.storage_identity().unwrap(), identity);
            let session = provider.open_session().unwrap();
            assert_eq!(session.backend.storage_identity().unwrap(), identity);
            let sibling = session.backend.open_session().unwrap();
            assert_eq!(sibling.backend.storage_identity().unwrap(), identity);
            session.backend.begin_read_transaction().unwrap();
            let retained = session
                .backend
                .open_retained_read_session(&uqa_core::CancellationToken::new())
                .unwrap();
            assert_eq!(retained.backend.storage_identity().unwrap(), identity);
            session.backend.rollback_transaction().unwrap();
        }
    }
}
