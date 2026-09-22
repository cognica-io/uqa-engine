//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Read-only adapters retain storage-owned snapshots and forward every read capability.

use std::sync::Arc;

use crate::StorageBackendError;

mod documents;
mod inverted;
mod vectors;

/// Adapt an already captured snapshot to an owned read-only handle without copying its contents. The caller must supply the snapshot selected for its visibility boundary; this adapter does not capture or advance that boundary.
pub struct ReadOnlySnapshot<T: ?Sized>(Arc<T>);

impl<T: ?Sized> ReadOnlySnapshot<T> {
    #[must_use]
    pub fn new(snapshot: Arc<T>) -> Self {
        Self(snapshot)
    }
}

impl<T: ?Sized> Clone for ReadOnlySnapshot<T> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

fn read_only_error() -> StorageBackendError {
    StorageBackendError::Other("cannot write a retained storage snapshot".into())
}

#[cfg(test)]
mod tests;
