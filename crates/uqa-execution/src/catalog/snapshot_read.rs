//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain an independent read transaction through catalog capture and release it on every exit.

use uqa_storage::{
    CatalogFacade, PersistentStorageBackend, PersistentStorageSession, StorageBackendError,
    StorageBackendResult,
};

pub(crate) fn with_read_transaction<T>(
    session: &PersistentStorageSession,
    read: impl FnOnce(&dyn CatalogFacade) -> StorageBackendResult<T>,
) -> StorageBackendResult<T> {
    session.validate_transaction_affinity()?;
    if session.backend.in_transaction() {
        return Err(StorageBackendError::Other(
            "catalog snapshot reads require an idle independent session".into(),
        ));
    }
    session.backend.begin_read_transaction()?;
    let mut transaction = ReadTransaction {
        backend: session.backend.as_ref(),
        active: true,
    };
    let result = session
        .backend
        .pin_transaction_snapshot()
        .and_then(|()| read(session.catalog.as_ref()));
    session.backend.rollback_transaction()?;
    transaction.active = false;
    result
}

struct ReadTransaction<'a> {
    backend: &'a dyn PersistentStorageBackend,
    active: bool,
}

impl Drop for ReadTransaction<'_> {
    fn drop(&mut self) {
        if self.active {
            let _ = self.backend.rollback_transaction();
        }
    }
}

#[cfg(test)]
mod tests;
