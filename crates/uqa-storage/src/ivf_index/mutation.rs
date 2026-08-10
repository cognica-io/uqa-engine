//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Validated IVF replacement, deletion, and reset operations.

use uqa_core::DocId;

use super::math::{l2_normalize, nearest_centroid};
use super::state::{IVFIndex, IVFState, StoredVector, VectorKey};
use crate::vector_index::validate_vector_values;
use crate::{StorageBackendError, StorageBackendResult};

impl IVFIndex {
    pub(super) fn replace_document_vectors(
        &mut self,
        doc_id: DocId,
        input_vectors: Vec<Vec<f32>>,
    ) -> StorageBackendResult<()> {
        for vector in &input_vectors {
            validate_vector_values(self.dimensions, vector)?;
        }
        validate_vector_ordinal_count(u64::try_from(input_vectors.len()).unwrap_or(u64::MAX))?;
        let centroids = self.centroids.lock().clone();
        let mut staged = Vec::with_capacity(input_vectors.len());
        for (ordinal, mut vector) in input_vectors.into_iter().enumerate() {
            let vector_ordinal = encode_vector_ordinal(ordinal)?;
            let raw_vector = vector.clone();
            let norm = l2_normalize(&mut vector);
            let centroid = (!centroids.is_empty()).then(|| nearest_centroid(&vector, &centroids));
            let key = (doc_id, vector_ordinal);
            staged.push(StoredVector {
                key,
                doc_id,
                vector_ordinal,
                raw_vector,
                norm,
                vector,
                centroid,
            });
        }

        let mut vectors = self.vectors.lock();
        let old_keys = vectors
            .keys()
            .filter(|(stored_doc_id, _)| *stored_doc_id == doc_id)
            .copied()
            .collect::<Vec<_>>();
        let old = old_keys
            .into_iter()
            .filter_map(|key| vectors.remove(&key).map(|vector| (key, vector.centroid)))
            .collect::<Vec<_>>();
        let additions = staged
            .iter()
            .filter_map(|vector| vector.centroid.map(|centroid| (vector.key, centroid)))
            .collect::<Vec<_>>();
        for vector in staged {
            vectors.insert(vector.key, vector);
        }
        let vector_count = vectors.len();
        drop(vectors);
        for (key, centroid) in old {
            if let Some(centroid) = centroid {
                self.remove_from_inverted_list(centroid, key);
            }
        }
        for (key, centroid) in additions {
            self.add_to_inverted_list(centroid, key);
        }
        if self.state() == IVFState::Untrained && vector_count >= self.train_threshold {
            self.train()?;
        }
        Ok(())
    }

    pub(super) fn delete_document(&mut self, doc_id: DocId) -> StorageBackendResult<()> {
        let mut vectors = self.vectors.lock();
        let keys = vectors
            .keys()
            .filter(|(stored_doc_id, _)| *stored_doc_id == doc_id)
            .copied()
            .collect::<Vec<VectorKey>>();
        if keys.is_empty() {
            return Ok(());
        }
        let tracks_trained_deletes = self.state() != IVFState::Untrained;
        let next_deletes = if tracks_trained_deletes {
            self.deletes_since_train
                .lock()
                .checked_add(keys.len())
                .ok_or_else(|| {
                    StorageBackendError::Other("IVF deletes-since-train counter overflow".into())
                })?
        } else {
            0
        };
        let removed = keys
            .into_iter()
            .filter_map(|key| vectors.remove(&key).map(|vector| (key, vector.centroid)))
            .collect::<Vec<_>>();
        drop(vectors);
        for (key, centroid) in removed {
            if let Some(centroid) = centroid {
                self.remove_from_inverted_list(centroid, key);
            }
        }
        *self.deletes_since_train.lock() = next_deletes;
        if tracks_trained_deletes {
            self.maybe_mark_stale();
        }
        Ok(())
    }

    pub(super) fn clear_index(&mut self) {
        self.vectors.lock().clear();
        self.centroids.lock().clear();
        self.inverted_lists.lock().clear();
        *self.state.lock() = IVFState::Untrained;
        *self.trained_size.lock() = 0;
        *self.deletes_since_train.lock() = 0;
    }
}

fn validate_vector_ordinal_count(count: u64) -> StorageBackendResult<()> {
    if count > u64::from(u32::MAX) + 1 {
        return Err(StorageBackendError::Other(
            "IVF vector ordinal exceeds the u32 index format".into(),
        ));
    }
    Ok(())
}

fn encode_vector_ordinal(ordinal: usize) -> StorageBackendResult<u32> {
    u32::try_from(ordinal).map_err(|_| {
        StorageBackendError::Other("IVF vector ordinal exceeds the u32 index format".into())
    })
}

#[cfg(test)]
pub(super) fn validate_vector_ordinal_count_for_test(count: u64) -> StorageBackendResult<()> {
    validate_vector_ordinal_count(count)
}
