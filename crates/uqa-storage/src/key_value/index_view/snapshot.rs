//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retained physical index snapshots never expose mutation, even after their live cache is gone.

use std::sync::Arc;

use uqa_core::{DocId, PostingList};

use crate::{StorageBackendError, StorageBackendResult, VectorIndex};

pub(in crate::key_value) fn read_only(index: Arc<dyn VectorIndex>) -> Arc<dyn VectorIndex> {
    Arc::new(Snapshot(index))
}

#[derive(Clone)]
struct Snapshot(Arc<dyn VectorIndex>);

impl VectorIndex for Snapshot {
    fn contains_document(&self, doc_id: DocId) -> StorageBackendResult<bool> {
        self.0.contains_document(doc_id)
    }

    fn dimensions(&self) -> u32 {
        self.0.dimensions()
    }
    fn index_kind(&self) -> &'static str {
        self.0.index_kind()
    }
    fn add(&mut self, _: DocId, _: Vec<f32>) -> StorageBackendResult<()> {
        Err(read_only_error())
    }
    fn add_many(&mut self, _: DocId, _: Vec<Vec<f32>>) -> StorageBackendResult<()> {
        Err(read_only_error())
    }
    fn delete(&mut self, _: DocId) -> StorageBackendResult<()> {
        Err(read_only_error())
    }
    fn clear(&mut self) -> StorageBackendResult<()> {
        Err(read_only_error())
    }
    fn initialize(&mut self) -> StorageBackendResult<()> {
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
    fn snapshot(&self) -> StorageBackendResult<Arc<dyn VectorIndex>> {
        Ok(Arc::new(self.clone()))
    }
}

fn read_only_error() -> StorageBackendError {
    StorageBackendError::Other("cannot write a retained KeyValue vector snapshot".into())
}
