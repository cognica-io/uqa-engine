//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Vector-index contract implementation over the HNSW graph.

use std::sync::Arc;

use uqa_core::{DocId, Payload, PostingEntry, PostingList};

use super::metric::normalize_with_norm;
use super::types::HNSWIndex;
use crate::vector_index::{
    cosine_similarity_with_norms, deduplicate_scored_by_doc, select_top_k_scored,
    validate_vector_values, vector_norm, VectorIndex,
};
use crate::{StorageBackendError, StorageBackendResult};

impl VectorIndex for HNSWIndex {
    fn dimensions(&self) -> u32 {
        self.dimensions
    }

    fn index_kind(&self) -> &'static str {
        "hnsw"
    }

    fn add(&mut self, doc_id: DocId, vector: Vec<f32>) -> StorageBackendResult<()> {
        self.add_many(doc_id, vec![vector])
    }

    fn add_many(&mut self, doc_id: DocId, vectors: Vec<Vec<f32>>) -> StorageBackendResult<()> {
        self.replace_document_vectors(doc_id, vectors)
    }

    fn delete(&mut self, doc_id: DocId) -> StorageBackendResult<()> {
        self.mark_document_deleted(doc_id)?;
        self.maybe_rebuild()
    }

    fn clear(&mut self) -> StorageBackendResult<()> {
        self.nodes.clear();
        self.active.clear();
        self.entry_point = None;
        self.max_level = 0;
        self.next_node_id = 1;
        self.deleted_count = 0;
        self.dirty_nodes.clear();
        self.full_rewrite = true;
        Ok(())
    }

    fn search_knn(&self, query: &[f32], k: usize) -> StorageBackendResult<PostingList> {
        validate_vector_values(self.dimensions, query)?;
        if k == 0 || self.active.is_empty() {
            return Ok(PostingList::new());
        }
        let (normalized_query, query_norm) = normalize_with_norm(query);
        let mut ef = self.params.ef_search.max(k).min(self.nodes.len());
        let mut scored = Vec::<(DocId, f32)>::with_capacity(ef);
        loop {
            scored.clear();
            for candidate in self.query_candidates(&normalized_query, ef) {
                let Some(node) = self.nodes.get(&candidate.node_id) else {
                    continue;
                };
                if node.deleted {
                    continue;
                }
                let score =
                    cosine_similarity_with_norms(query, &node.raw_vector, query_norm, node.norm);
                scored.push((node.doc_id, score));
            }
            deduplicate_scored_by_doc(&mut scored);
            if scored.len() >= k || ef >= self.nodes.len() {
                break;
            }
            ef = ef
                .checked_mul(2)
                .unwrap_or(self.nodes.len())
                .min(self.nodes.len());
        }
        select_top_k_scored(&mut scored, k);
        scored.sort_by_key(|(doc_id, _)| *doc_id);
        Ok(PostingList::from_sorted_unchecked(
            scored
                .into_iter()
                .map(|(doc_id, score)| {
                    PostingEntry::new(doc_id, Payload::with_score(f64::from(score)))
                })
                .collect(),
        ))
    }

    fn search_threshold(&self, query: &[f32], threshold: f32) -> StorageBackendResult<PostingList> {
        validate_vector_values(self.dimensions, query)?;
        if !threshold.is_finite() {
            return Err(StorageBackendError::Other(format!(
                "vector similarity threshold must be finite, got {threshold}"
            )));
        }
        let query_norm = vector_norm(query);
        let mut scored = Vec::<(DocId, f32)>::new();
        for node_id in self.active.values() {
            let node = self.nodes.get(node_id).ok_or_else(|| {
                StorageBackendError::Other(format!(
                    "HNSW active map references missing node {node_id}"
                ))
            })?;
            let score =
                cosine_similarity_with_norms(query, &node.raw_vector, query_norm, node.norm);
            if score >= threshold {
                scored.push((node.doc_id, score));
            }
        }
        deduplicate_scored_by_doc(&mut scored);
        Ok(PostingList::from_sorted_unchecked(
            scored
                .into_iter()
                .map(|(doc_id, score)| {
                    PostingEntry::new(doc_id, Payload::with_score(f64::from(score)))
                })
                .collect(),
        ))
    }

    fn count(&self) -> StorageBackendResult<usize> {
        Ok(self.active.len())
    }

    fn snapshot(&self) -> StorageBackendResult<Arc<dyn VectorIndex>> {
        Ok(Arc::new(self.clone()))
    }

    fn writable_snapshot(&self) -> StorageBackendResult<Box<dyn VectorIndex>> {
        Ok(Box::new(self.clone()))
    }
}
