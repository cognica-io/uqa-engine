//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Tuple keys and current-definition links publish or roll back together.

use super::{
    PersistentStorageSession, StorageBackendError, StorageBackendResult, StorageSavepointId,
};

pub(super) fn require_active(session: &PersistentStorageSession) -> StorageBackendResult<()> {
    session.validate_transaction_affinity()?;
    if !session.backend.in_transaction() {
        return Err(StorageBackendError::Other(
            "catalog tuples require an active definition transaction".into(),
        ));
    }
    Ok(())
}

pub(super) fn atomic<T>(
    session: &PersistentStorageSession,
    operation: impl FnOnce() -> StorageBackendResult<T>,
) -> StorageBackendResult<T> {
    require_active(session)?;
    let savepoint = StorageSavepointId::allocate();
    session.backend.savepoint(savepoint)?;
    let result = operation().and_then(|value| {
        session.backend.release_savepoint(savepoint)?;
        Ok(value)
    });
    match result {
        Ok(value) => Ok(value),
        Err(original) => {
            let cleanup = session
                .backend
                .rollback_to_savepoint(savepoint)
                .and_then(|()| session.backend.release_savepoint(savepoint));
            match cleanup {
                Ok(()) => Err(original),
                Err(cleanup) => Err(StorageBackendError::backend(
                    "catalog tuple rollback",
                    RollbackFailure { original, cleanup },
                )),
            }
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{original}; rolling back the catalog tuple publication also failed: {cleanup}")]
struct RollbackFailure {
    #[source]
    original: StorageBackendError,
    cleanup: StorageBackendError,
}
