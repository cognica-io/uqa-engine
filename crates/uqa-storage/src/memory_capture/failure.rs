//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Repeated observations preserve a cut's original terminal resource failure.

use std::sync::Arc;
use uqa_core::memory::MemoryError;

use crate::StorageBackendError;

pub(super) fn reproduce(error: &Arc<StorageBackendError>) -> StorageBackendError {
    match &**error {
        StorageBackendError::Memory(MemoryError::Limit { required, limit }) => MemoryError::Limit {
            required: *required,
            limit: *limit,
        }
        .into(),
        StorageBackendError::Memory(MemoryError::SizeOverflow) => MemoryError::SizeOverflow.into(),
        StorageBackendError::Memory(MemoryError::Allocation(error)) => {
            MemoryError::Allocation(error.clone()).into()
        }
        StorageBackendError::Cancelled(error) => (*error).into(),
        _ => StorageBackendError::backend("memory capture", SharedFailure(error.clone())),
    }
}

#[derive(Debug)]
struct SharedFailure(Arc<StorageBackendError>);

impl std::fmt::Display for SharedFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

impl std::error::Error for SharedFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&*self.0)
    }
}
