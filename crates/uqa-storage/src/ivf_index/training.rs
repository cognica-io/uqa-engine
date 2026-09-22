//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! IVF state transitions, centroid training, and posting-list maintenance.

use super::math::{kmeans, nearest_centroid_controlled};
use super::prepare::{check, workspace_bytes};
use super::state::{IVFIndex, IVFState, VectorKey};
use crate::read_control::StorageReadControl;
use crate::{StorageBackendError, StorageBackendResult};

pub(super) const STALE_DENOMINATOR: usize = 5;

impl IVFIndex {
    pub(super) fn train_for_query(
        &self,
        control: Option<&StorageReadControl>,
    ) -> StorageBackendResult<()> {
        // Every reader retains its own training workspace. The cached generation's construction lease cannot cover scratch used by concurrent queries.
        let _workspace = if let Some(control) = control {
            control.check()?;
            let count = self.vectors.lock().len();
            let clusters = self.centroids.lock().len().max(self.nlist.min(count));
            Some(
                control
                    .memory()
                    .reserve(workspace_bytes(self.dimensions, count, clusters)?)?,
            )
        } else {
            None
        };
        self.train_controlled(control)
    }

    pub fn train(&self) -> StorageBackendResult<()> {
        self.train_controlled(None)
    }

    pub(super) fn train_controlled(
        &self,
        control: Option<&StorageReadControl>,
    ) -> StorageBackendResult<()> {
        check(control)?;
        let training_vectors = {
            let vectors = self.vectors.lock();
            if vectors.len() < self.train_threshold {
                drop(vectors);
                self.transition_to_untrained(control)?;
                return Ok(());
            }
            let mut training = Vec::with_capacity(vectors.len());
            for vector in vectors.values() {
                check(control)?;
                training.push(vector.vector.clone());
            }
            training
        };
        let dimensions = usize::try_from(self.dimensions).map_err(|_| {
            StorageBackendError::Other(format!(
                "IVF dimension {} exceeds the addressable memory range",
                self.dimensions
            ))
        })?;
        let centroids = kmeans(
            &training_vectors,
            self.nlist.min(training_vectors.len()),
            dimensions,
            10,
            control,
        )?;
        let mut vectors = self.vectors.lock();
        let mut inverted_lists = vec![Vec::new(); centroids.len()];
        let mut assignments = Vec::with_capacity(vectors.len());
        for vector in vectors.values() {
            let centroid = nearest_centroid_controlled(&vector.vector, &centroids, control)?;
            assignments.push(centroid);
            inverted_lists[centroid].push(vector.key);
        }
        check(control)?;
        // Publish only after all allocation, numerical evaluation and cancellation checks succeed.
        for (vector, centroid) in vectors.values_mut().zip(assignments) {
            vector.centroid = Some(centroid);
        }
        *self.centroids.lock() = centroids;
        *self.inverted_lists.lock() = inverted_lists;
        *self.trained_size.lock() = vectors.len();
        *self.deletes_since_train.lock() = 0;
        *self.state.lock() = IVFState::Trained;
        Ok(())
    }

    fn transition_to_untrained(
        &self,
        control: Option<&StorageReadControl>,
    ) -> StorageBackendResult<()> {
        let mut vectors = self.vectors.lock();
        for _ in vectors.values() {
            check(control)?;
        }
        for vector in vectors.values_mut() {
            vector.centroid = None;
        }
        self.centroids.lock().clear();
        self.inverted_lists.lock().clear();
        *self.trained_size.lock() = 0;
        *self.deletes_since_train.lock() = 0;
        *self.state.lock() = IVFState::Untrained;
        Ok(())
    }

    pub(super) fn maybe_mark_stale(&self) {
        let trained = *self.trained_size.lock();
        if trained > 0 && *self.deletes_since_train.lock() > trained / STALE_DENOMINATOR {
            *self.state.lock() = IVFState::Stale;
        }
    }

    pub(super) fn remove_from_inverted_list(&self, centroid: usize, key: VectorKey) {
        if let Some(list) = self.inverted_lists.lock().get_mut(centroid) {
            list.retain(|candidate| *candidate != key);
        }
    }

    pub(super) fn add_to_inverted_list(&self, centroid: usize, key: VectorKey) {
        if let Some(list) = self.inverted_lists.lock().get_mut(centroid) {
            if let Err(position) = list.binary_search(&key) {
                list.insert(position, key);
            }
        }
    }
}
