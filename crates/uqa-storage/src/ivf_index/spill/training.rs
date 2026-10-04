//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! K-means scans spilled normalized vectors without a corpus-sized training copy.

use super::{IVFPreparedMetadata, IVFState, StoredVector};
use crate::{
    ivf_index::math::{kmeans_source, nearest_centroid_controlled},
    spill_map::Map,
    StorageBackendResult,
};
use uqa_core::memory::MemoryError;

impl IVFPreparedMetadata {
    pub(super) fn train(&mut self) -> StorageBackendResult<()> {
        self.control.check()?;
        let count = self.vectors.len();
        let clusters = if count < self.params.train_threshold {
            0
        } else {
            self.params.nlist.min(count)
        };
        let coordinates = usize::try_from(self.dimensions)
            .map_err(|_| MemoryError::SizeOverflow)?
            .checked_mul(4)
            .ok_or(MemoryError::SizeOverflow)?;
        let retained = clusters
            .checked_mul(
                coordinates
                    .checked_add(size_of::<Vec<f32>>())
                    .ok_or(MemoryError::SizeOverflow)?,
            )
            .ok_or(MemoryError::SizeOverflow)?;
        let workspace = retained
            .checked_mul(2)
            .and_then(|bytes| bytes.checked_add(clusters.checked_mul(size_of::<usize>())?))
            .ok_or(MemoryError::SizeOverflow)?;
        let mut memory = self.control.memory().reserve(workspace)?;
        let centroids = kmeans_source(
            count,
            clusters,
            self.dimensions as usize,
            10,
            Some(&self.control),
            &mut |visitor| {
                for (position, entry) in self.vectors.iter().enumerate() {
                    self.control.check()?;
                    let (_, vector) = entry?;
                    visitor(position, &vector.vector)?;
                }
                Ok(())
            },
        )?;
        let mut assigned =
            Map::<StoredVector>::new(self.control.memory(), self.control.memory().limit() / 3);
        for entry in self.vectors.iter() {
            self.control.check()?;
            let (key, vector) = entry?;
            let bytes = vector
                .raw_vector
                .len()
                .checked_add(vector.vector.len())
                .and_then(|count| count.checked_mul(4))
                .and_then(|bytes| bytes.checked_add(size_of::<StoredVector>()))
                .ok_or(MemoryError::SizeOverflow)?;
            let _memory = self.control.memory().reserve(bytes)?;
            let mut value = (*vector).clone();
            value.centroid = if clusters == 0 {
                None
            } else {
                Some(nearest_centroid_controlled(
                    &value.vector,
                    &centroids,
                    Some(&self.control),
                )?)
            };
            assigned.insert(key, value, Some(&self.control))?;
        }
        self.control.check()?;
        self.snapshot.centroids = centroids;
        self.memory = memory.split(retained);
        self.vectors = assigned;
        self.snapshot.state = if clusters == 0 {
            IVFState::Untrained
        } else {
            IVFState::Trained
        };
        self.snapshot.trained_size = if clusters == 0 { 0 } else { count };
        self.snapshot.deletes_since_train = 0;
        self.snapshot.vector_count = count;
        Ok(())
    }
}
