//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Public vector-index contract for IVF.

use std::sync::Arc;

use uqa_core::{DocId, PostingList};

use super::state::IVFIndex;
use crate::vector_index::VectorIndex;
use crate::StorageBackendResult;

impl VectorIndex for IVFIndex {
    fn dimensions(&self) -> u32 {
        self.dimensions
    }

    fn index_kind(&self) -> &'static str {
        "ivf"
    }

    fn add(&mut self, doc_id: DocId, vector: Vec<f32>) -> StorageBackendResult<()> {
        self.replace_document_vectors(doc_id, vec![vector])
    }

    fn add_many(&mut self, doc_id: DocId, vectors: Vec<Vec<f32>>) -> StorageBackendResult<()> {
        self.replace_document_vectors(doc_id, vectors)
    }

    fn delete(&mut self, doc_id: DocId) -> StorageBackendResult<()> {
        self.delete_document(doc_id)
    }

    fn clear(&mut self) -> StorageBackendResult<()> {
        self.clear_index();
        Ok(())
    }

    fn search_knn(&self, query: &[f32], k: usize) -> StorageBackendResult<PostingList> {
        self.search_top_k(query, k)
    }

    fn search_threshold(&self, query: &[f32], threshold: f32) -> StorageBackendResult<PostingList> {
        self.search_above_threshold(query, threshold)
    }

    fn count(&self) -> StorageBackendResult<usize> {
        Ok(self.vectors.lock().len())
    }

    fn initialize(&mut self) -> StorageBackendResult<()> {
        self.train()
    }

    fn snapshot(&self) -> StorageBackendResult<Arc<dyn VectorIndex>> {
        Ok(Arc::new(self.detached_clone()))
    }

    fn writable_snapshot(&self) -> StorageBackendResult<Box<dyn VectorIndex>> {
        Ok(Box::new(self.detached_clone()))
    }
}
