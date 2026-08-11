//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `VectorIndex` lifecycle implementation for persistent HNSW.

use std::sync::Arc;

use uqa_core::{DocId, PostingList};

use super::SQLiteHNSWIndex;
use crate::vector_index::VectorIndex;
use crate::StorageBackendResult;

impl VectorIndex for SQLiteHNSWIndex {
    fn dimensions(&self) -> u32 {
        self.persistent.dimensions
    }

    fn index_kind(&self) -> &'static str {
        "hnsw"
    }

    fn add(&mut self, doc_id: DocId, vector: Vec<f32>) -> StorageBackendResult<()> {
        self.add_many(doc_id, vec![vector])
    }

    fn add_many(&mut self, doc_id: DocId, vectors: Vec<Vec<f32>>) -> StorageBackendResult<()> {
        self.replace_document(doc_id, vectors)
    }

    fn delete(&mut self, doc_id: DocId) -> StorageBackendResult<()> {
        self.delete_document(doc_id)
    }

    fn clear(&mut self) -> StorageBackendResult<()> {
        self.clear_graph()
    }

    fn search_knn(&self, query: &[f32], k: usize) -> StorageBackendResult<PostingList> {
        self.search_top_k(query, k)
    }

    fn search_threshold(&self, query: &[f32], threshold: f32) -> StorageBackendResult<PostingList> {
        self.persistent.search_threshold(query, threshold)
    }

    fn count(&self) -> StorageBackendResult<usize> {
        self.persistent.count()
    }

    fn initialize(&mut self) -> StorageBackendResult<()> {
        self.initialize_graph()
    }

    fn snapshot(&self) -> StorageBackendResult<Arc<dyn VectorIndex>> {
        if let Some(revision) = self.persisted_revision()? {
            Ok(self.cached_graph_for_revision(revision)?)
        } else if self.require_persisted_graph {
            Err(super::mutation::missing_metadata(self))
        } else {
            self.persistent.snapshot()
        }
    }
}
