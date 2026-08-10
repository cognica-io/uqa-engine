//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Checked reconstruction of IVF state from canonical vectors and metadata.

use std::collections::{BTreeMap, BTreeSet};

use uqa_core::DocId;

use super::math::l2_normalize;
use super::state::{IVFIndex, IVFMetadataSnapshot, IVFState, StoredVector};
use crate::vector_index::validate_vector_values;
use crate::{StorageBackendError, StorageBackendResult};

impl IVFIndex {
    pub(crate) fn from_persistence(
        dimensions: u32,
        nlist: usize,
        nprobe: usize,
        train_threshold: usize,
        vectors: Vec<(DocId, u32, Vec<f32>)>,
        snapshot: IVFMetadataSnapshot,
    ) -> StorageBackendResult<Self> {
        if snapshot.vector_count != vectors.len() {
            return Err(corrupt(format!(
                "metadata vector_count {} does not match {} canonical vectors",
                snapshot.vector_count,
                vectors.len()
            )));
        }
        for centroid in &snapshot.centroids {
            validate_vector_values(dimensions, centroid)
                .map_err(|error| corrupt(format!("invalid centroid: {error}")))?;
        }
        if snapshot.centroids.len() > nlist {
            return Err(corrupt(format!(
                "{} centroids exceed configured nlist {nlist}",
                snapshot.centroids.len()
            )));
        }
        validate_state_shape(&snapshot)?;

        let assignments = snapshot
            .assignments
            .iter()
            .map(|(doc_id, ordinal, centroid)| ((*doc_id, *ordinal), *centroid))
            .collect::<BTreeMap<_, _>>();
        if assignments.len() != snapshot.assignments.len() {
            return Err(corrupt("duplicate vector assignment"));
        }
        let mut persisted = BTreeMap::new();
        let mut seen = BTreeSet::new();
        for (doc_id, ordinal, raw_vector) in vectors {
            validate_vector_values(dimensions, &raw_vector)
                .map_err(|error| corrupt(format!("invalid canonical vector: {error}")))?;
            if !seen.insert((doc_id, ordinal)) {
                return Err(corrupt(format!(
                    "duplicate canonical vector {doc_id}:{ordinal}"
                )));
            }
            let centroid = assignments.get(&(doc_id, ordinal)).copied();
            if let Some(centroid) = centroid {
                if centroid >= snapshot.centroids.len() {
                    return Err(corrupt(format!(
                        "vector {doc_id}:{ordinal} references missing centroid {centroid}"
                    )));
                }
            } else if snapshot.state != IVFState::Untrained {
                return Err(corrupt(format!(
                    "trained vector {doc_id}:{ordinal} has no assignment"
                )));
            }
            let mut vector = raw_vector.clone();
            let norm = l2_normalize(&mut vector);
            persisted.insert(
                (doc_id, ordinal),
                StoredVector {
                    key: (doc_id, ordinal),
                    doc_id,
                    vector_ordinal: ordinal,
                    norm,
                    raw_vector,
                    vector,
                    centroid,
                },
            );
        }
        if assignments.keys().any(|key| !seen.contains(key)) {
            return Err(corrupt("assignment references a missing canonical vector"));
        }
        let mut inverted_lists = vec![Vec::new(); snapshot.centroids.len()];
        for (key, vector) in &persisted {
            if let Some(centroid) = vector.centroid {
                inverted_lists[centroid].push(*key);
            }
        }
        Ok(Self {
            dimensions,
            nlist,
            nprobe: parking_lot::Mutex::new(nprobe),
            train_threshold,
            state: parking_lot::Mutex::new(snapshot.state),
            vectors: parking_lot::Mutex::new(persisted),
            centroids: parking_lot::Mutex::new(snapshot.centroids),
            inverted_lists: parking_lot::Mutex::new(inverted_lists),
            trained_size: parking_lot::Mutex::new(snapshot.trained_size),
            deletes_since_train: parking_lot::Mutex::new(snapshot.deletes_since_train),
        })
    }
}

fn validate_state_shape(snapshot: &IVFMetadataSnapshot) -> StorageBackendResult<()> {
    match snapshot.state {
        IVFState::Untrained => {
            if !snapshot.centroids.is_empty()
                || !snapshot.assignments.is_empty()
                || snapshot.trained_size != 0
                || snapshot.deletes_since_train != 0
            {
                return Err(corrupt("untrained metadata contains trained state"));
            }
        }
        IVFState::Trained | IVFState::Stale => {
            if snapshot.centroids.is_empty() {
                return Err(corrupt("trained metadata has no centroids"));
            }
            if snapshot.assignments.len() != snapshot.vector_count {
                return Err(corrupt(format!(
                    "{} assignments do not cover {} vectors",
                    snapshot.assignments.len(),
                    snapshot.vector_count
                )));
            }
        }
    }
    Ok(())
}

fn corrupt(message: impl std::fmt::Display) -> StorageBackendError {
    StorageBackendError::Other(format!("corrupt IVF index: {message}"))
}
