//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_core::memory::MemoryError;

/// Backup errors never include image payloads, credentials or encryption keys.
#[derive(Debug, thiserror::Error)]
pub enum BackupError {
    #[error("invalid backup encoding: {0}")]
    Invalid(&'static str),
    #[error("unsupported backup format or capability: {0}")]
    Unsupported(&'static str),
    #[error("backup integrity verification failed")]
    Integrity,
    #[error("backup operation is no longer writable or readable")]
    Finished,
    #[error("backup {resource} requires {required} bytes, exceeding limit {limit}")]
    Limit {
        resource: &'static str,
        required: u64,
        limit: u64,
    },
    #[error("backup identity generation failed")]
    Random,
    #[error(transparent)]
    Memory(#[from] MemoryError),
    #[error(transparent)]
    Cancelled(#[from] uqa_core::QueryCancelled),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Storage(crate::StorageBackendError),
}

pub type BackupResult<T> = Result<T, BackupError>;

impl From<crate::mvcc::VersionError> for BackupError {
    fn from(error: crate::mvcc::VersionError) -> Self {
        error.into_storage_error().into()
    }
}

impl From<crate::StorageBackendError> for BackupError {
    fn from(error: crate::StorageBackendError) -> Self {
        match error {
            crate::StorageBackendError::Memory(error) => Self::Memory(error),
            crate::StorageBackendError::Cancelled(error) => Self::Cancelled(error),
            error => Self::Storage(error),
        }
    }
}
