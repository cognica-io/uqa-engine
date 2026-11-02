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
    #[error("backup repository operation is busy")]
    Busy,
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
    Io(std::io::Error),
    #[error(transparent)]
    Storage(crate::StorageBackendError),
}

pub type BackupResult<T> = Result<T, BackupError>;

impl From<std::io::Error> for BackupError {
    fn from(error: std::io::Error) -> Self {
        if error
            .get_ref()
            .is_some_and(<dyn std::error::Error + Send + Sync>::is::<Self>)
        {
            *error
                .into_inner()
                .expect("owned backup stream error")
                .downcast::<Self>()
                .expect("checked backup error type")
        } else {
            Self::Io(error)
        }
    }
}

impl From<crate::StorageBackendError> for BackupError {
    fn from(error: crate::StorageBackendError) -> Self {
        if let crate::StorageBackendError::Backend { source, .. } = &error {
            if let Some(limit) = source
                .downcast_ref::<std::io::Error>()
                .and_then(std::io::Error::get_ref)
                .and_then(|error| error.downcast_ref::<crate::temporary_file::FileLimit>())
            {
                return Self::Limit {
                    resource: "verification workspace",
                    required: limit.required,
                    limit: limit.limit,
                };
            }
        }
        match error {
            crate::StorageBackendError::Memory(error) => Self::Memory(error),
            crate::StorageBackendError::Cancelled(error) => Self::Cancelled(error),
            error => Self::Storage(error),
        }
    }
}
