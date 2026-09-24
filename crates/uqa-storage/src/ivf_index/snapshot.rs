//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Detached captures retain one complete training generation and their original allowance.

use std::convert::Infallible;

use uqa_core::memory::Budgeted;

use super::{prepare::workspace_bytes, IVFIndex};
use crate::{read_control::StorageReadControl, StorageBackendResult};

impl IVFIndex {
    pub(super) fn snapshot_controlled(
        &self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Budgeted<Self>> {
        control.check()?;
        let count = self.vectors.lock().len();
        let clusters = self.centroids.lock().len().max(self.nlist.min(count));
        let memory =
            control
                .memory()
                .reserve(workspace_bytes(self.dimensions, count, clusters)?)?;
        Ok(Budgeted::new(self.clone_controlled(control)?, memory))
    }

    pub(crate) fn detached_clone(&self) -> Self {
        match self.clone_checked(|| Ok::<(), Infallible>(())) {
            Ok(candidate) => candidate,
            Err(never) => match never {},
        }
    }

    pub(super) fn clone_controlled(
        &self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        self.clone_checked(|| control.check())
    }

    fn clone_checked<E>(&self, mut check: impl FnMut() -> Result<(), E>) -> Result<Self, E> {
        check()?;
        // Training publishes assignments, centroids, lists and counters while holding the vectors lock. Keep the same lock through capture so readers never combine different generations.
        let vectors = self.vectors.lock();
        let mut candidate = Self::with_params(
            self.dimensions,
            self.nlist,
            self.nprobe(),
            self.train_threshold,
        );
        for (key, vector) in vectors.iter() {
            check()?;
            candidate.vectors.get_mut().insert(*key, vector.clone());
        }
        let centroids = self.centroids.lock();
        *candidate.centroids.get_mut() = Vec::with_capacity(centroids.len());
        for centroid in centroids.iter() {
            check()?;
            candidate.centroids.get_mut().push(centroid.clone());
        }
        drop(centroids);
        let lists = self.inverted_lists.lock();
        *candidate.inverted_lists.get_mut() = Vec::with_capacity(lists.len());
        for list in lists.iter() {
            check()?;
            candidate.inverted_lists.get_mut().push(list.clone());
        }
        check()?;
        *candidate.state.get_mut() = self.state();
        *candidate.trained_size.get_mut() = *self.trained_size.lock();
        *candidate.deletes_since_train.get_mut() = *self.deletes_since_train.lock();
        Ok(candidate)
    }
}
