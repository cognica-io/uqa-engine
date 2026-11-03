//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Memory vector registrations retain source identity independently of mutable index membership.

use super::{
    image::VectorImageSnapshot, VectorIndex, VectorIndexSpec, VectorQueryResult, VectorReadSnapshot,
};
use crate::{
    memory_capture::{
        MemoryCapture, MemoryCaptureCell, MemoryCaptureKey, MemoryCaptureRegistry,
        MemoryCaptureSource,
    },
    read_control::StorageReadControl,
    StorageBackendResult,
};
use std::sync::Arc;
use uqa_core::{memory::Budgeted, DocId, PostingList};

/// Read the selected image, or preserve it before the live registration leaves its collection. Implementations own all logical mutations, including changes made through shared query references.
pub trait VectorIndexCapture: Send + Sync {
    fn key(&self) -> StorageBackendResult<MemoryCaptureKey<VectorImageSnapshot>>;
    fn captured(
        &self,
        capture: &MemoryCapture,
    ) -> StorageBackendResult<Arc<Budgeted<VectorImageSnapshot>>>;
    fn retire(&self);
}

/// Adapter for concrete memory owners whose logical mutations require `&mut self`. IVF supplies its own boundary because queries can train it through a shared reference.
pub(crate) struct ExclusiveVectorCapture {
    index: MemoryCaptureCell<Box<dyn VectorIndex>>,
}

impl ExclusiveVectorCapture {
    pub(crate) fn register(
        index: Box<dyn VectorIndex>,
        registry: &MemoryCaptureRegistry,
    ) -> StorageBackendResult<Box<dyn VectorIndex>> {
        Ok(Box::new(Self {
            index: registry.cell(index)?,
        }))
    }
}

impl MemoryCaptureSource for Box<dyn VectorIndex> {
    type Frozen = VectorImageSnapshot;
    fn freeze(&self, control: &StorageReadControl) -> StorageBackendResult<Self::Frozen> {
        self.capture_image_with_control(control)
    }
}

impl VectorIndexCapture for ExclusiveVectorCapture {
    fn key(&self) -> StorageBackendResult<MemoryCaptureKey<VectorImageSnapshot>> {
        self.index.key()
    }
    fn captured(
        &self,
        capture: &MemoryCapture,
    ) -> StorageBackendResult<Arc<Budgeted<VectorImageSnapshot>>> {
        self.index.captured(capture)
    }
    fn retire(&self) {
        drop(self.index.write());
    }
}

impl VectorIndex for ExclusiveVectorCapture {
    fn into_memory_capture(
        mut self: Box<Self>,
        registry: &MemoryCaptureRegistry,
    ) -> StorageBackendResult<Box<dyn VectorIndex>> {
        self.index.register(registry)?;
        Ok(self)
    }
    fn memory_capture(&self) -> Option<&dyn VectorIndexCapture> {
        Some(self)
    }
    fn capture_image_with_control(
        &self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<VectorImageSnapshot> {
        self.index
            .read_with_control(control)?
            .capture_image_with_control(control)
    }
    fn write_image(
        &self,
        output: &mut dyn std::io::Write,
        control: &StorageReadControl,
    ) -> StorageBackendResult<u64> {
        self.index
            .read_with_control(control)?
            .write_image(output, control)
    }
    fn dimensions(&self) -> u32 {
        self.index.read().dimensions()
    }
    fn index_kind(&self) -> &'static str {
        self.index.read().index_kind()
    }
    fn physical_configuration(&self) -> Option<VectorIndexSpec> {
        self.index.read().physical_configuration()
    }
    fn diskann_query_metadata(
        &self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<crate::diskann_index::DiskANNQueryMetadata>> {
        self.index
            .read_with_control(control)?
            .diskann_query_metadata(control)
    }
    fn add(&mut self, doc_id: DocId, vector: Vec<f32>) -> StorageBackendResult<()> {
        self.index.write().add(doc_id, vector)
    }
    fn add_many(&mut self, doc_id: DocId, vectors: Vec<Vec<f32>>) -> StorageBackendResult<()> {
        self.index.write().add_many(doc_id, vectors)
    }
    fn delete(&mut self, doc_id: DocId) -> StorageBackendResult<()> {
        self.index.write().delete(doc_id)
    }
    fn clear(&mut self) -> StorageBackendResult<()> {
        self.index.write().clear()
    }
    fn initialize(&mut self) -> StorageBackendResult<()> {
        self.index.write().initialize()
    }
    fn count(&self) -> StorageBackendResult<usize> {
        self.index.read().count()
    }
    fn contains_document(&self, doc_id: DocId) -> StorageBackendResult<bool> {
        self.index.read().contains_document(doc_id)
    }
    fn search_knn(&self, query: &[f32], k: usize) -> StorageBackendResult<PostingList> {
        self.index.read().search_knn(query, k)
    }
    fn search_threshold(&self, query: &[f32], threshold: f32) -> StorageBackendResult<PostingList> {
        self.index.read().search_threshold(query, threshold)
    }
    fn search_knn_with_control(
        &self,
        query: &[f32],
        k: usize,
        control: &StorageReadControl,
    ) -> StorageBackendResult<PostingList> {
        self.index
            .read_with_control(control)?
            .search_knn_with_control(query, k, control)
    }
    fn search_threshold_with_control(
        &self,
        query: &[f32],
        threshold: f32,
        control: &StorageReadControl,
    ) -> StorageBackendResult<PostingList> {
        self.index
            .read_with_control(control)?
            .search_threshold_with_control(query, threshold, control)
    }
    fn search_knn_with_statistics(
        &self,
        query: &[f32],
        k: usize,
        control: Option<&StorageReadControl>,
    ) -> StorageBackendResult<VectorQueryResult> {
        match control {
            Some(control) => self
                .index
                .read_with_control(control)?
                .search_knn_with_statistics(query, k, Some(control)),
            None => self.index.read().search_knn_with_statistics(query, k, None),
        }
    }
    fn search_threshold_with_statistics(
        &self,
        query: &[f32],
        threshold: f32,
        control: Option<&StorageReadControl>,
    ) -> StorageBackendResult<VectorQueryResult> {
        match control {
            Some(control) => self
                .index
                .read_with_control(control)?
                .search_threshold_with_statistics(query, threshold, Some(control)),
            None => self
                .index
                .read()
                .search_threshold_with_statistics(query, threshold, None),
        }
    }
    fn snapshot(&self) -> StorageBackendResult<Arc<dyn VectorIndex>> {
        self.index.read().snapshot()
    }
    fn snapshot_with_control(
        &self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Arc<dyn VectorIndex>> {
        self.index
            .read_with_control(control)?
            .snapshot_with_control(control)
    }
    fn writable_snapshot(&self) -> StorageBackendResult<Box<dyn VectorIndex>> {
        self.index.read().writable_snapshot()
    }
    fn diskann_read_snapshot(
        &self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<crate::diskann_index::DiskANNReadSnapshot>> {
        self.index
            .read_with_control(control)?
            .diskann_read_snapshot(control)
    }
    fn vector_read_snapshot(
        &self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<VectorReadSnapshot>> {
        self.index
            .read_with_control(control)?
            .vector_read_snapshot(control)
    }
    fn matches_document_vectors(
        &self,
        document: DocId,
        vectors: &[Vec<f32>],
        control: &StorageReadControl,
    ) -> StorageBackendResult<bool> {
        self.index
            .read_with_control(control)?
            .matches_document_vectors(document, vectors, control)
    }
    fn snapshot_with_vector_read(
        &self,
        source: VectorReadSnapshot,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<Arc<dyn VectorIndex>>> {
        self.index
            .read_with_control(control)?
            .snapshot_with_vector_read(source, control)
    }
    fn snapshot_with_diskann_changes(
        &self,
        changes: &crate::diskann_index::DiskANNReadChanges,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<Arc<dyn VectorIndex>>> {
        self.index
            .read_with_control(control)?
            .snapshot_with_diskann_changes(changes, control)
    }
}
