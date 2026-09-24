//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical HNSW queries retain invocation-owned workspace without changing graph traversal.

use uqa_core::{DocId, PostingList};

use super::{prepare::Control, HNSWIndex};
use crate::{
    vector_index::{
        cosine_similarity_with_norms, deduplicate_scored_values,
        query::{
            check, normalized_query, postings_from_scores, postings_from_unique_scores,
            VectorQueryBuffer,
        },
        validate_vector_values, vector_norm,
    },
    StorageBackendError, StorageBackendResult,
};

impl HNSWIndex {
    pub(super) fn search_top_k(
        &self,
        query: &[f32],
        k: usize,
        control: Control<'_>,
    ) -> StorageBackendResult<PostingList> {
        check(control)?;
        validate_vector_values(self.dimensions, query)?;
        if k == 0 || self.active.is_empty() {
            return Ok(PostingList::new());
        }
        let (normalized, norm) = normalized_query(query, control)?;
        let mut ef = self.params.ef_search.max(k).min(self.nodes.len());
        let mut scored = VectorQueryBuffer::<(DocId, f32)>::new(control);
        scored.reserve(ef)?;
        loop {
            scored.clear();
            let candidates = self.query_candidates(&normalized, ef, control)?;
            for candidate in candidates.iter() {
                check(control)?;
                let Some(node) = self.nodes.get(&candidate.node_id) else {
                    continue;
                };
                if !node.deleted {
                    let score =
                        cosine_similarity_with_norms(query, &node.raw_vector, norm, node.norm);
                    scored.push((node.doc_id, score))?;
                }
            }
            drop(candidates);
            let count = deduplicate_scored_values(&mut scored);
            scored.truncate(count);
            if scored.len() >= k || ef >= self.nodes.len() {
                return postings_from_unique_scores(scored, Some(k), control);
            }
            ef = ef
                .checked_mul(2)
                .unwrap_or(self.nodes.len())
                .min(self.nodes.len());
        }
    }

    pub(super) fn search_above_threshold(
        &self,
        query: &[f32],
        threshold: f32,
        control: Control<'_>,
    ) -> StorageBackendResult<PostingList> {
        check(control)?;
        validate_vector_values(self.dimensions, query)?;
        if !threshold.is_finite() {
            return Err(StorageBackendError::Other(format!(
                "vector similarity threshold must be finite, got {threshold}"
            )));
        }
        let norm = vector_norm(query);
        let mut scored = VectorQueryBuffer::new(control);
        for node_id in self.active.values() {
            check(control)?;
            let node = self.nodes.get(node_id).ok_or_else(|| {
                StorageBackendError::Other(format!(
                    "HNSW active map references missing node {node_id}"
                ))
            })?;
            let score = cosine_similarity_with_norms(query, &node.raw_vector, norm, node.norm);
            if score >= threshold {
                scored.push((node.doc_id, score))?;
            }
        }
        postings_from_scores(scored, None, control)
    }
}
