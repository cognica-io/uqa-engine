//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Vector-index contract implementation over the HNSW graph.

use std::sync::Arc;

use uqa_core::{DocId, PostingList};

use super::types::HNSWIndex;
use crate::vector_index::VectorIndex;
use crate::{read_control::StorageReadControl, StorageBackendResult};

impl VectorIndex for HNSWIndex {
    fn contains_document(&self, doc_id: DocId) -> StorageBackendResult<bool> {
        Ok(self.active.contains_key(&(doc_id, 0)))
    }

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
        self.replace_document_vectors(doc_id, vectors, None)
    }

    fn delete(&mut self, doc_id: DocId) -> StorageBackendResult<()> {
        self.mark_document_deleted(doc_id, None)?;
        self.maybe_rebuild(None)
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
        self.search_top_k(query, k, None)
    }

    fn search_threshold(&self, query: &[f32], threshold: f32) -> StorageBackendResult<PostingList> {
        self.search_above_threshold(query, threshold, None)
    }

    fn search_knn_with_control(
        &self,
        query: &[f32],
        k: usize,
        control: &StorageReadControl,
    ) -> StorageBackendResult<PostingList> {
        self.search_top_k(query, k, Some(control))
    }

    fn search_threshold_with_control(
        &self,
        query: &[f32],
        threshold: f32,
        control: &StorageReadControl,
    ) -> StorageBackendResult<PostingList> {
        self.search_above_threshold(query, threshold, Some(control))
    }

    fn count(&self) -> StorageBackendResult<usize> {
        Ok(self.active.len())
    }

    fn snapshot(&self) -> StorageBackendResult<Arc<dyn VectorIndex>> {
        Ok(Arc::new(self.clone()))
    }

    fn snapshot_with_control(
        &self,
        control: &crate::read_control::StorageReadControl,
    ) -> StorageBackendResult<Arc<dyn VectorIndex>> {
        crate::ReadOnlySnapshot::from_budgeted(self.snapshot_controlled(control)?)?
            .with_vector_read_control(control)?
            .snapshot()
    }

    fn writable_snapshot(&self) -> StorageBackendResult<Box<dyn VectorIndex>> {
        Ok(Box::new(self.clone()))
    }
}
