//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! IVF state transitions, centroid training, and posting-list maintenance.

use super::math::{kmeans, nearest_centroid_controlled};
use super::prepare::{check, workspace_bytes};
use super::state::{IVFData, IVFState, VectorKey};
use crate::read_control::StorageReadControl;
use crate::{StorageBackendError, StorageBackendResult};

pub(super) const STALE_DENOMINATOR: usize = 5;

impl IVFData {
    pub(super) fn train_for_query(
        &mut self,
        control: Option<&StorageReadControl>,
    ) -> StorageBackendResult<()> {
        // Every reader retains its own training workspace. The cached generation's construction lease cannot cover scratch used by concurrent queries.
        let _workspace = if let Some(control) = control {
            control.check()?;
            let count = self.vectors.len();
            let clusters = self.centroids.len().max(self.nlist.min(count));
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

    pub fn train(&mut self) -> StorageBackendResult<()> {
        self.train_controlled(None)
    }

    pub(super) fn train_controlled(
        &mut self,
        control: Option<&StorageReadControl>,
    ) -> StorageBackendResult<()> {
        check(control)?;
        let training_vectors = {
            let vectors = &mut self.vectors;
            if vectors.len() < self.train_threshold {
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
        let vectors = &mut self.vectors;
        let mut inverted_lists = vec![Vec::new(); centroids.len()];
        let mut assignments = Vec::with_capacity(vectors.len());
        for vector in vectors.values() {
            let centroid = nearest_centroid_controlled(&vector.vector, &centroids, control)?;
            assignments.push(centroid);
            inverted_lists[centroid].push(vector.key);
        }
        check(control)?;
        // The exclusive owner publishes only after allocation, evaluation and cancellation succeed.
        for (vector, centroid) in vectors.values_mut().zip(assignments) {
            vector.centroid = Some(centroid);
        }
        self.centroids = centroids;
        self.inverted_lists = inverted_lists;
        self.trained_size = vectors.len();
        self.deletes_since_train = 0;
        self.state = IVFState::Trained;
        Ok(())
    }

    fn transition_to_untrained(
        &mut self,
        control: Option<&StorageReadControl>,
    ) -> StorageBackendResult<()> {
        let vectors = &mut self.vectors;
        for _ in vectors.values() {
            check(control)?;
        }
        for vector in vectors.values_mut() {
            vector.centroid = None;
        }
        self.centroids.clear();
        self.inverted_lists.clear();
        self.trained_size = 0;
        self.deletes_since_train = 0;
        self.state = IVFState::Untrained;
        Ok(())
    }

    pub(super) fn maybe_mark_stale(&mut self) {
        let trained = self.trained_size;
        if trained > 0 && self.deletes_since_train > trained / STALE_DENOMINATOR {
            self.state = IVFState::Stale;
        }
    }

    pub(super) fn remove_from_inverted_list(&mut self, centroid: usize, key: VectorKey) {
        if let Some(list) = self.inverted_lists.get_mut(centroid) {
            list.retain(|candidate| *candidate != key);
        }
    }

    pub(super) fn add_to_inverted_list(&mut self, centroid: usize, key: VectorKey) {
        if let Some(list) = self.inverted_lists.get_mut(centroid) {
            if let Err(position) = list.binary_search(&key) {
                list.insert(position, key);
            }
        }
    }
}
