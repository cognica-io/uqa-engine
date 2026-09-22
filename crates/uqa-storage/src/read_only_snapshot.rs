//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Read-only adapters retain storage-owned snapshots and forward every read capability.

use std::sync::Arc;
use uqa_core::memory::MemoryReservation;

use crate::StorageBackendError;

mod documents;
mod inverted;
mod vectors;

/// Adapt an already captured snapshot to an owned read-only handle without copying its contents. The caller must supply the snapshot selected for its visibility boundary; this adapter does not capture or advance that boundary.
pub struct ReadOnlySnapshot<T: ?Sized>(Arc<T>, Option<Arc<MemoryReservation>>);

impl<T: ?Sized> ReadOnlySnapshot<T> {
    #[must_use]
    pub fn new(snapshot: Arc<T>) -> Self {
        Self(snapshot, None)
    }

    pub(crate) fn with_retention(
        snapshot: Arc<T>,
        memory: MemoryReservation,
    ) -> crate::StorageBackendResult<Self> {
        // Keep the snapshot ahead of its lease on failure, including the shared lease allocation.
        let mut pending = (snapshot, memory);
        let shared = pending.1.budget().reserve(size_of::<MemoryReservation>())?;
        pending.1.absorb(shared);
        Ok(Self(pending.0, Some(Arc::new(pending.1))))
    }
}

impl<T: ?Sized> Clone for ReadOnlySnapshot<T> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0), self.1.as_ref().map(Arc::clone))
    }
}

fn read_only_error() -> StorageBackendError {
    StorageBackendError::Other("cannot write a retained storage snapshot".into())
}

#[cfg(test)]
mod tests;
