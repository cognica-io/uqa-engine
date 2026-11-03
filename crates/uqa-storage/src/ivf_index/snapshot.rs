//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Detached captures retain one complete training generation and their original allowance.

use std::convert::Infallible;

use uqa_core::memory::Budgeted;

use super::{prepare::workspace_bytes, IVFData};
use crate::{read_control::StorageReadControl, StorageBackendResult};

impl IVFData {
    pub(super) fn snapshot_controlled(
        &self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Budgeted<Self>> {
        control.check()?;
        let count = self.vectors.len();
        let clusters = self.centroids.len().max(self.nlist.min(count));
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
        // The immutable borrow comes from the index's single state boundary, so all dependent fields belong to one training generation.
        let vectors = &self.vectors;
        let mut candidate = Self::with_params(
            self.dimensions,
            self.nlist,
            self.nprobe(),
            self.train_threshold,
        );
        for (key, vector) in vectors {
            check()?;
            candidate.vectors.insert(*key, vector.clone());
        }
        let centroids = &self.centroids;
        candidate.centroids = Vec::with_capacity(centroids.len());
        for centroid in centroids {
            check()?;
            candidate.centroids.push(centroid.clone());
        }

        let lists = &self.inverted_lists;
        candidate.inverted_lists = Vec::with_capacity(lists.len());
        for list in lists {
            check()?;
            candidate.inverted_lists.push(list.clone());
        }
        check()?;
        candidate.state = self.state();
        candidate.trained_size = self.trained_size;
        candidate.deletes_since_train = self.deletes_since_train;
        Ok(candidate)
    }
}
