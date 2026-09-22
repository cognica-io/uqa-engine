//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::sync::Arc;

use uqa_core::{DocId, PostingList};

use crate::{StorageBackendResult, VectorIndex};

use super::{read_only_error, ReadOnlySnapshot};

impl VectorIndex for ReadOnlySnapshot<dyn VectorIndex> {
    fn dimensions(&self) -> u32 {
        self.0.dimensions()
    }

    fn index_kind(&self) -> &'static str {
        self.0.index_kind()
    }

    fn add(&mut self, _doc_id: DocId, _vector: Vec<f32>) -> StorageBackendResult<()> {
        Err(read_only_error())
    }

    fn add_many(&mut self, _doc_id: DocId, _vectors: Vec<Vec<f32>>) -> StorageBackendResult<()> {
        Err(read_only_error())
    }

    fn delete(&mut self, _doc_id: DocId) -> StorageBackendResult<()> {
        Err(read_only_error())
    }

    fn clear(&mut self) -> StorageBackendResult<()> {
        Err(read_only_error())
    }

    fn search_knn(&self, query: &[f32], k: usize) -> StorageBackendResult<PostingList> {
        self.0.search_knn(query, k)
    }

    fn search_threshold(&self, query: &[f32], threshold: f32) -> StorageBackendResult<PostingList> {
        self.0.search_threshold(query, threshold)
    }

    fn count(&self) -> StorageBackendResult<usize> {
        self.0.count()
    }

    fn contains_document(&self, doc_id: DocId) -> StorageBackendResult<bool> {
        self.0.contains_document(doc_id)
    }

    fn initialize(&mut self) -> StorageBackendResult<()> {
        Err(read_only_error())
    }

    fn snapshot(&self) -> StorageBackendResult<Arc<dyn VectorIndex>> {
        Ok(Arc::new(self.clone()))
    }

    fn snapshot_with_control(
        &self,
        control: &crate::read_control::StorageReadControl,
    ) -> StorageBackendResult<Arc<dyn VectorIndex>> {
        Ok(Arc::new(Self(
            self.0.snapshot_with_control(control)?,
            self.1.as_ref().map(Arc::clone),
        )))
    }
}
