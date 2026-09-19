//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Validated IVF replacement, deletion, and reset operations.

use uqa_core::DocId;

use super::math::{l2_normalize, nearest_centroid_controlled};
use super::prepare::check;
use super::state::{IVFIndex, IVFState, StoredVector, VectorKey};
use crate::read_control::StorageReadControl;
use crate::vector_index::validate_vector_values;
use crate::{StorageBackendError, StorageBackendResult};

impl IVFIndex {
    pub(super) fn replace_document_vectors(
        &mut self,
        doc_id: DocId,
        input_vectors: Vec<Vec<f32>>,
    ) -> StorageBackendResult<()> {
        self.replace_controlled(doc_id, input_vectors, None)
    }

    pub(super) fn replace_controlled(
        &mut self,
        doc_id: DocId,
        input_vectors: Vec<Vec<f32>>,
        control: Option<&StorageReadControl>,
    ) -> StorageBackendResult<()> {
        for vector in &input_vectors {
            check(control)?;
            validate_vector_values(self.dimensions, vector)?;
        }
        validate_vector_ordinal_count(u64::try_from(input_vectors.len()).unwrap_or(u64::MAX))?;
        let centroids = self.centroids.lock();
        let mut staged = Vec::with_capacity(input_vectors.len());
        for (ordinal, mut vector) in input_vectors.into_iter().enumerate() {
            check(control)?;
            let vector_ordinal = encode_vector_ordinal(ordinal)?;
            let raw_vector = vector.clone();
            let norm = l2_normalize(&mut vector);
            let centroid = (!centroids.is_empty())
                .then(|| nearest_centroid_controlled(&vector, &centroids, control))
                .transpose()?;
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
        drop(centroids);

        let mut vectors = self.vectors.lock();
        let old_keys = document_keys(&vectors, doc_id, control)?;
        let mut old = Vec::with_capacity(old_keys.len());
        for key in old_keys {
            check(control)?;
            if let Some(vector) = vectors.remove(&key) {
                old.push((key, vector.centroid));
            }
        }
        let mut additions = Vec::with_capacity(staged.len());
        for vector in &staged {
            check(control)?;
            if let Some(centroid) = vector.centroid {
                additions.push((vector.key, centroid));
            }
        }
        for vector in staged {
            check(control)?;
            vectors.insert(vector.key, vector);
        }
        let vector_count = vectors.len();
        drop(vectors);
        for (key, centroid) in old {
            check(control)?;
            if let Some(centroid) = centroid {
                self.remove_from_inverted_list(centroid, key);
            }
        }
        for (key, centroid) in additions {
            check(control)?;
            self.add_to_inverted_list(centroid, key);
        }
        if self.state() == IVFState::Untrained && vector_count >= self.train_threshold {
            self.train_controlled(control)?;
        }
        Ok(())
    }

    pub(super) fn delete_document(&mut self, doc_id: DocId) -> StorageBackendResult<()> {
        self.delete_controlled(doc_id, None)
    }

    pub(super) fn delete_controlled(
        &mut self,
        doc_id: DocId,
        control: Option<&StorageReadControl>,
    ) -> StorageBackendResult<()> {
        check(control)?;
        let mut vectors = self.vectors.lock();
        let keys = document_keys(&vectors, doc_id, control)?;
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
        let mut removed = Vec::with_capacity(keys.len());
        for key in keys {
            check(control)?;
            if let Some(vector) = vectors.remove(&key) {
                removed.push((key, vector.centroid));
            }
        }
        drop(vectors);
        for (key, centroid) in removed {
            check(control)?;
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

fn document_keys(
    vectors: &std::collections::BTreeMap<VectorKey, StoredVector>,
    document: DocId,
    control: Option<&StorageReadControl>,
) -> StorageBackendResult<Vec<VectorKey>> {
    let range = (document, 0)..=(document, u32::MAX);
    let mut count = 0;
    for _ in vectors.range(range.clone()) {
        check(control)?;
        count += 1;
    }
    let mut keys = Vec::with_capacity(count);
    for (key, _) in vectors.range(range) {
        check(control)?;
        keys.push(*key);
    }
    Ok(keys)
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
