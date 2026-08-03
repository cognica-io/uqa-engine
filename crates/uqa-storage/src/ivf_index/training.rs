//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! IVF state transitions, centroid training, and posting-list maintenance.

use super::math::{kmeans, nearest_centroid};
use super::state::{IVFIndex, IVFState, VectorKey};
use crate::{StorageBackendError, StorageBackendResult};

pub(super) const STALE_DENOMINATOR: usize = 5;

impl IVFIndex {
    pub fn train(&self) -> StorageBackendResult<()> {
        let training_vectors = {
            let vectors = self.vectors.lock();
            if vectors.len() < self.train_threshold {
                drop(vectors);
                self.transition_to_untrained();
                return Ok(());
            }
            vectors
                .values()
                .map(|vector| vector.vector.clone())
                .collect::<Vec<_>>()
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
        );
        let mut vectors = self.vectors.lock();
        let mut inverted_lists = vec![Vec::new(); centroids.len()];
        for vector in vectors.values_mut() {
            let centroid = nearest_centroid(&vector.vector, &centroids);
            vector.centroid = Some(centroid);
            inverted_lists[centroid].push(vector.key);
        }
        *self.centroids.lock() = centroids;
        *self.inverted_lists.lock() = inverted_lists;
        *self.trained_size.lock() = vectors.len();
        *self.deletes_since_train.lock() = 0;
        *self.state.lock() = IVFState::Trained;
        Ok(())
    }

    fn transition_to_untrained(&self) {
        for vector in self.vectors.lock().values_mut() {
            vector.centroid = None;
        }
        self.centroids.lock().clear();
        self.inverted_lists.lock().clear();
        *self.trained_size.lock() = 0;
        *self.deletes_since_train.lock() = 0;
        *self.state.lock() = IVFState::Untrained;
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
