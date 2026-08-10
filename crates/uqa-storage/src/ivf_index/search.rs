//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Approximate top-k and exact threshold search over IVF state.

use uqa_core::{DocId, Payload, PostingEntry, PostingList};

use super::math::{dot, l2_normalize};
use super::state::{IVFIndex, IVFState, StoredVector};
use crate::vector_index::{
    cosine_similarity_with_norms, deduplicate_scored_by_doc, select_top_k_scored,
    validate_vector_values, vector_norm,
};
use crate::{StorageBackendError, StorageBackendResult};

impl IVFIndex {
    pub(super) fn search_top_k(
        &self,
        query: &[f32],
        k: usize,
    ) -> StorageBackendResult<PostingList> {
        validate_vector_values(self.dimensions, query)?;
        if k == 0 {
            return Ok(PostingList::new());
        }
        let mut normalized_query = query.to_vec();
        let query_norm = l2_normalize(&mut normalized_query);
        if self.state() == IVFState::Stale {
            self.train()?;
        }
        let state = self.state();
        let vectors = self.vectors.lock();
        let mut scored = Vec::<(DocId, f32)>::new();
        let score = |vector: &StoredVector, output: &mut Vec<(DocId, f32)>| {
            output.push((
                vector.doc_id,
                cosine_similarity_with_norms(query, &vector.raw_vector, query_norm, vector.norm),
            ));
        };
        match state {
            IVFState::Untrained => {
                for vector in vectors.values() {
                    score(vector, &mut scored);
                }
            }
            IVFState::Trained | IVFState::Stale => {
                let centroids = self.centroids.lock();
                if centroids.is_empty() {
                    for vector in vectors.values() {
                        score(vector, &mut scored);
                    }
                } else {
                    let mut centroid_scores = centroids
                        .iter()
                        .enumerate()
                        .map(|(index, centroid)| (index, dot(&normalized_query, centroid)))
                        .collect::<Vec<_>>();
                    centroid_scores.sort_by(|left, right| right.1.total_cmp(&left.1));
                    let probes = centroid_scores
                        .into_iter()
                        .take(self.nprobe())
                        .map(|(index, _)| index)
                        .collect::<Vec<_>>();
                    let lists = self.inverted_lists.lock();
                    for centroid in probes {
                        for key in lists.get(centroid).into_iter().flatten() {
                            if let Some(vector) = vectors.get(key) {
                                score(vector, &mut scored);
                            }
                        }
                    }
                }
            }
        }
        Ok(posting_list_from_scores(scored, Some(k)))
    }

    pub(super) fn search_above_threshold(
        &self,
        query: &[f32],
        threshold: f32,
    ) -> StorageBackendResult<PostingList> {
        validate_vector_values(self.dimensions, query)?;
        if !threshold.is_finite() {
            return Err(StorageBackendError::Other(format!(
                "vector similarity threshold must be finite, got {threshold}"
            )));
        }
        let query_norm = vector_norm(query);
        let scored = self
            .vectors
            .lock()
            .values()
            .filter_map(|vector| {
                let similarity = cosine_similarity_with_norms(
                    query,
                    &vector.raw_vector,
                    query_norm,
                    vector.norm,
                );
                (similarity >= threshold).then_some((vector.doc_id, similarity))
            })
            .collect::<Vec<_>>();
        Ok(posting_list_from_scores(scored, None))
    }
}

fn posting_list_from_scores(scored: Vec<(DocId, f32)>, limit: Option<usize>) -> PostingList {
    let mut scored = scored;
    deduplicate_scored_by_doc(&mut scored);
    if let Some(limit) = limit {
        select_top_k_scored(&mut scored, limit);
    }
    scored.sort_by_key(|(doc_id, _)| *doc_id);
    PostingList::from_sorted_unchecked(
        scored
            .into_iter()
            .map(|(doc_id, score)| PostingEntry::new(doc_id, Payload::with_score(f64::from(score))))
            .collect(),
    )
}
