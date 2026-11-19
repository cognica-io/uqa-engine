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
    fn into_memory_capture(
        self: Box<Self>,
        registry: &crate::memory_capture::MemoryCaptureRegistry,
    ) -> StorageBackendResult<Box<dyn VectorIndex>> {
        crate::vector_index::capture::ExclusiveVectorCapture::register(self, registry, None)
    }

    fn into_memory_capture_with_control(
        self: Box<Self>,
        registry: &crate::memory_capture::MemoryCaptureRegistry,
        control: &crate::read_control::StorageReadControl,
    ) -> StorageBackendResult<Box<dyn VectorIndex>> {
        crate::vector_index::capture::ExclusiveVectorCapture::register(
            self,
            registry,
            Some(control),
        )
    }

    fn write_image(
        &self,
        output: &mut dyn std::io::Write,
        control: &StorageReadControl,
    ) -> StorageBackendResult<u64> {
        super::image::write(self, output, control)
    }

    fn image_reader<'a>(
        &'a self,
        control: &'a StorageReadControl,
    ) -> StorageBackendResult<crate::image_io::ImageReader<'a>> {
        super::image::reader(self, control)
    }

    fn contains_document(&self, doc_id: DocId) -> StorageBackendResult<bool> {
        Ok(self
            .active
            .get(super::types::active_key(doc_id, 0))?
            .is_some())
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
        let control = StorageReadControl::new(&self.memory, &uqa_core::CancellationToken::new());
        let _workspace = super::prepare::workspace::operation(self, &control)?;
        let mut candidate = self.clone();
        candidate.replace_document_vectors(doc_id, &vectors, Some(&control))?;
        *self = candidate;
        Ok(())
    }

    fn delete(&mut self, doc_id: DocId) -> StorageBackendResult<()> {
        let control = StorageReadControl::new(&self.memory, &uqa_core::CancellationToken::new());
        let _workspace = super::prepare::workspace::operation(self, &control)?;
        let mut candidate = self.clone();
        candidate.mark_document_deleted(doc_id, Some(&control))?;
        candidate.maybe_rebuild(Some(&control))?;
        *self = candidate;
        Ok(())
    }

    fn clear(&mut self) -> StorageBackendResult<()> {
        self.nodes.clear();
        self.raw_vectors.clear();
        self.normalized_vectors.clear();
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
        let control = StorageReadControl::new(&self.memory, &uqa_core::CancellationToken::new());
        self.search_top_k(query, k, Some(&control))
    }

    fn search_threshold(&self, query: &[f32], threshold: f32) -> StorageBackendResult<PostingList> {
        let control = StorageReadControl::new(&self.memory, &uqa_core::CancellationToken::new());
        self.search_above_threshold(query, threshold, Some(&control))
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
        crate::ReadOnlySnapshot::new(Arc::new(self.clone()))
            .with_canonical_vectors(None)?
            .snapshot()
    }

    fn snapshot_with_control(
        &self,
        control: &crate::read_control::StorageReadControl,
    ) -> StorageBackendResult<Arc<dyn VectorIndex>> {
        crate::ReadOnlySnapshot::from_budgeted(self.snapshot_controlled(control)?)?
            .with_canonical_vectors(Some(control))?
            .with_vector_read_control(control)?
            .snapshot()
    }

    fn vector_read_snapshot(
        &self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<crate::vector_index::VectorReadSnapshot>> {
        self.snapshot_with_control(control)?
            .vector_read_snapshot(control)
    }

    fn matches_document_vectors(
        &self,
        document: DocId,
        vectors: &[Vec<f32>],
        control: &StorageReadControl,
    ) -> StorageBackendResult<bool> {
        crate::vector_index::canonical_vectors_equal(self, document, vectors, control)
    }

    fn writable_snapshot(&self) -> StorageBackendResult<Box<dyn VectorIndex>> {
        Ok(Box::new(self.clone()))
    }
}
