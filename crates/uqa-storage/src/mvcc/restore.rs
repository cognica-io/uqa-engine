//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Caller-retained identities distinguish one explicit backup restoration from an ordinary reopen.

use super::{DatabaseId, VersionError, VersionResult};

/// One restoration of a closed, consistent backup into a new database incarnation. Persist this request outside the database before calling a provider's restore entry point, and reuse it only to resolve that same operation after an error. A separate restoration, including another copy of the same backup, requires a fresh target identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DatabaseRestore {
    source: DatabaseId,
    target: DatabaseId,
}

impl DatabaseRestore {
    /// Allocate the new incarnation without opening or changing a database.
    pub fn new(source: DatabaseId) -> VersionResult<Self> {
        let mut target = [0; 16];
        getrandom::fill(&mut target).map_err(|error| {
            crate::StorageBackendError::Other(format!(
                "allocate database restore identity: {error}"
            ))
        })?;
        Self::from_identities(source, DatabaseId::from_bytes(target))
    }

    /// Reconstitute a previously retained request. `target` must be unique to this restoration and must differ from the backup's incarnation; callers normally obtain it from `new`.
    pub fn from_identities(source: DatabaseId, target: DatabaseId) -> VersionResult<Self> {
        if source == target {
            return Err(VersionError::InvalidRestoreIdentity);
        }
        Ok(Self { source, target })
    }

    pub const fn source(self) -> DatabaseId {
        self.source
    }

    pub const fn target(self) -> DatabaseId {
        self.target
    }

    /// A provider must inspect this before changing any restore state. Seeing the target means the atomic transition already completed; retries must preserve all subsequent transactions.
    pub fn needs_restore(self, current: DatabaseId) -> VersionResult<bool> {
        if current == self.source {
            Ok(true)
        } else if current == self.target {
            Ok(false)
        } else {
            Err(VersionError::WrongDatabase)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restored_history_requests_distinguish_source_completion_and_foreign_databases() {
        let source = DatabaseId::from_bytes([1; 16]);
        let target = DatabaseId::from_bytes([2; 16]);
        let request = DatabaseRestore::from_identities(source, target).unwrap();
        assert!(request.needs_restore(source).unwrap());
        assert!(!request.needs_restore(target).unwrap());
        assert!(matches!(
            request.needs_restore(DatabaseId::from_bytes([3; 16])),
            Err(VersionError::WrongDatabase)
        ));
        assert!(matches!(
            DatabaseRestore::from_identities(source, source),
            Err(VersionError::InvalidRestoreIdentity)
        ));
        let generated = DatabaseRestore::new(source).unwrap();
        assert_eq!(generated.source(), source);
        assert_ne!(generated.target(), source);
        assert_eq!(
            DatabaseRestore::from_identities(generated.source(), generated.target()).unwrap(),
            generated
        );
    }
}
