//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Read-only adapters retain storage-owned snapshots and forward every read capability.

use std::sync::Arc;
use uqa_core::memory::{Budgeted, MemoryReservation};

use crate::StorageBackendError;

mod documents;
mod inverted;
mod vectors;

/// Adapt an already captured snapshot to an owned read-only handle without copying its contents. The caller must supply the snapshot selected for its visibility boundary; this adapter does not capture or advance that boundary.
pub struct ReadOnlySnapshot<T: ?Sized>(
    Arc<T>,
    Option<Arc<MemoryReservation>>,
    Option<crate::read_control::StorageReadControl>,
);

impl<T> ReadOnlySnapshot<T> {
    /// Share a captured value without separating its allocations from their original allowance.
    pub fn from_budgeted(snapshot: Budgeted<T>) -> crate::StorageBackendResult<Self> {
        let mut pending = snapshot.into_parts();
        let shared = pending.1.budget().reserve(size_of::<T>())?;
        pending.1.absorb(shared);
        Self::with_retention(Arc::new(pending.0), pending.1)
    }
}

impl<T: ?Sized> ReadOnlySnapshot<T> {
    #[must_use]
    pub fn new(snapshot: Arc<T>) -> Self {
        Self(snapshot, None, None)
    }

    pub(crate) fn with_retention(
        snapshot: Arc<T>,
        memory: MemoryReservation,
    ) -> crate::StorageBackendResult<Self> {
        // Keep the snapshot ahead of its lease on failure, including the shared lease allocation.
        let mut pending = (snapshot, memory);
        let shared = pending.1.budget().reserve(size_of::<MemoryReservation>())?;
        pending.1.absorb(shared);
        Ok(Self::with_shared_retention(pending.0, Arc::new(pending.1)))
    }

    /// The owning value can retain the same lease so direct owner snapshots cannot separate shared data from its allowance.
    pub(crate) fn with_shared_retention(snapshot: Arc<T>, memory: Arc<MemoryReservation>) -> Self {
        Self(snapshot, Some(memory), None)
    }
}

impl<T: ?Sized> Clone for ReadOnlySnapshot<T> {
    fn clone(&self) -> Self {
        Self(
            Arc::clone(&self.0),
            self.1.as_ref().map(Arc::clone),
            self.2.clone(),
        )
    }
}

impl<T: ?Sized> std::ops::Deref for ReadOnlySnapshot<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.0
    }
}

fn read_only_error() -> StorageBackendError {
    StorageBackendError::Other("cannot write a retained storage snapshot".into())
}

#[cfg(test)]
mod tests;
