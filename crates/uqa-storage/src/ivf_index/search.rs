//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Approximate top-k and exact threshold search retain each reader's workspace allowance.

use uqa_core::{DocId, PostingList};

use super::state::{IVFIndex, IVFState, StoredVector};
use crate::read_control::StorageReadControl;
use crate::vector_index::{
    cosine_similarity_with_norms,
    query::{
        check, nearest_normalized_centroids, normalized_query, postings_from_scores,
        VectorQueryBuffer,
    },
    validate_vector_values, vector_norm,
};
use crate::{StorageBackendError, StorageBackendResult};

impl IVFIndex {
    pub(super) fn search_top_k(
        &self,
        query: &[f32],
        k: usize,
        control: Option<&StorageReadControl>,
    ) -> StorageBackendResult<PostingList> {
        check(control)?;
        validate_vector_values(self.dimensions, query)?;
        if k == 0 {
            return Ok(PostingList::new());
        }
        let (normalized, norm) = normalized_query(query, control)?;
        if self.state() == IVFState::Stale {
            self.train_for_query(control)?;
        }
        let vectors = self.vectors.lock();
        let state = self.state();
        let mut scored = VectorQueryBuffer::<(DocId, f32)>::new(control);
        let score = |vector: &StoredVector,
                     output: &mut VectorQueryBuffer<(DocId, f32)>|
         -> StorageBackendResult<()> {
            check(control)?;
            output.push((
                vector.doc_id,
                cosine_similarity_with_norms(query, &vector.raw_vector, norm, vector.norm),
            ))
        };
        match state {
            IVFState::Untrained => {
                for vector in vectors.values() {
                    score(vector, &mut scored)?;
                }
            }
            IVFState::Trained | IVFState::Stale => {
                let centroids = self.centroids.lock();
                if centroids.is_empty() {
                    for vector in vectors.values() {
                        score(vector, &mut scored)?;
                    }
                } else {
                    let probes = nearest_normalized_centroids(
                        &normalized,
                        &centroids,
                        self.nprobe(),
                        control,
                    )?;
                    let lists = self.inverted_lists.lock();
                    for &centroid in probes.iter() {
                        for key in lists.get(centroid).into_iter().flatten() {
                            check(control)?;
                            if let Some(vector) = vectors.get(key) {
                                score(vector, &mut scored)?;
                            }
                        }
                    }
                }
            }
        }
        postings_from_scores(scored, Some(k), control)
    }

    pub(super) fn search_above_threshold(
        &self,
        query: &[f32],
        threshold: f32,
        control: Option<&StorageReadControl>,
    ) -> StorageBackendResult<PostingList> {
        check(control)?;
        validate_vector_values(self.dimensions, query)?;
        if !threshold.is_finite() {
            return Err(StorageBackendError::Other(format!(
                "vector similarity threshold must be finite, got {threshold}"
            )));
        }
        let norm = vector_norm(query);
        let vectors = self.vectors.lock();
        let mut scored = VectorQueryBuffer::new(control);
        for vector in vectors.values() {
            check(control)?;
            let similarity =
                cosine_similarity_with_norms(query, &vector.raw_vector, norm, vector.norm);
            if similarity >= threshold {
                scored.push((vector.doc_id, similarity))?;
            }
        }
        postings_from_scores(scored, None, control)
    }
}
