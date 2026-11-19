//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Public vector-index contract for IVF.

use std::sync::Arc;

use uqa_core::{DocId, PostingList};

use super::IVFIndex;
use crate::memory_capture::MemoryCaptureSource;
use crate::vector_index::VectorIndex;
use crate::StorageBackendResult;

impl VectorIndex for IVFIndex {
    fn into_memory_capture(
        mut self: Box<Self>,
        registry: &crate::memory_capture::MemoryCaptureRegistry,
    ) -> StorageBackendResult<Box<dyn VectorIndex>> {
        self.register_memory_capture(registry)?;
        Ok(self)
    }

    fn into_memory_capture_with_control(
        mut self: Box<Self>,
        registry: &crate::memory_capture::MemoryCaptureRegistry,
        control: &crate::read_control::StorageReadControl,
    ) -> StorageBackendResult<Box<dyn VectorIndex>> {
        self.data.register_with_control(registry, control)?;
        Ok(self)
    }

    fn memory_capture(&self) -> Option<&dyn crate::vector_index::VectorIndexCapture> {
        Some(self)
    }

    fn capture_image_with_control(
        &self,
        control: &crate::read_control::StorageReadControl,
    ) -> StorageBackendResult<crate::vector_index::image::VectorImageSnapshot> {
        self.data.read_with_control(control)?.freeze(control)
    }

    fn image_reader<'a>(
        &'a self,
        control: &'a crate::read_control::StorageReadControl,
    ) -> StorageBackendResult<crate::image_io::ImageReader<'a>> {
        self.capture_image_with_control(control)?
            .into_image_reader(control)
    }

    fn write_image(
        &self,
        output: &mut dyn std::io::Write,
        control: &crate::read_control::StorageReadControl,
    ) -> StorageBackendResult<u64> {
        super::image::write(&*self.data.read_with_control(control)?, output, control)
    }

    fn contains_document(&self, doc_id: DocId) -> StorageBackendResult<bool> {
        Ok(self.data.read().vectors.contains_key(&(doc_id, 0)))
    }

    fn dimensions(&self) -> u32 {
        self.data.read().dimensions
    }

    fn index_kind(&self) -> &'static str {
        "ivf"
    }

    fn add(&mut self, doc_id: DocId, vector: Vec<f32>) -> StorageBackendResult<()> {
        self.data
            .write()
            .replace_document_vectors(doc_id, vec![vector])
    }

    fn add_many(&mut self, doc_id: DocId, vectors: Vec<Vec<f32>>) -> StorageBackendResult<()> {
        self.data.write().replace_document_vectors(doc_id, vectors)
    }

    fn delete(&mut self, doc_id: DocId) -> StorageBackendResult<()> {
        self.data.write().delete_document(doc_id)
    }

    fn clear(&mut self) -> StorageBackendResult<()> {
        self.data.write().clear_index();
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
        control: &crate::read_control::StorageReadControl,
    ) -> StorageBackendResult<PostingList> {
        self.search_top_k(query, k, Some(control))
    }

    fn search_threshold_with_control(
        &self,
        query: &[f32],
        threshold: f32,
        control: &crate::read_control::StorageReadControl,
    ) -> StorageBackendResult<PostingList> {
        self.search_above_threshold(query, threshold, Some(control))
    }

    fn count(&self) -> StorageBackendResult<usize> {
        Ok(self.data.read().vectors.len())
    }

    fn initialize(&mut self) -> StorageBackendResult<()> {
        self.train()
    }

    fn snapshot(&self) -> StorageBackendResult<Arc<dyn VectorIndex>> {
        crate::ReadOnlySnapshot::new(Arc::new(self.detached_clone()))
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
        control: &crate::read_control::StorageReadControl,
    ) -> StorageBackendResult<Option<crate::vector_index::VectorReadSnapshot>> {
        self.snapshot_with_control(control)?
            .vector_read_snapshot(control)
    }

    fn matches_document_vectors(
        &self,
        document: DocId,
        vectors: &[Vec<f32>],
        control: &crate::read_control::StorageReadControl,
    ) -> StorageBackendResult<bool> {
        crate::vector_index::canonical_vectors_equal(self, document, vectors, control)
    }

    fn writable_snapshot(&self) -> StorageBackendResult<Box<dyn VectorIndex>> {
        Ok(Box::new(self.detached_clone()))
    }
}
