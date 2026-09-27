//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::sync::Arc;

use uqa_core::{DocId, PostingList};

use crate::read_control::StorageReadControl;
use crate::{StorageBackendResult, VectorIndex};

use super::{read_only_error, ReadOnlySnapshot};

impl<T: VectorIndex + ?Sized> ReadOnlySnapshot<T> {
    /// Select query control for an already captured physical value. Cache owners bind each returned reader separately; nested readers preserve the first selected allowance and cancellation token.
    pub fn with_vector_read_control(
        mut self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        if self.2.is_none() {
            self.2 = Some(control.clone());
        }
        Ok(self)
    }
}

impl<T: VectorIndex + ?Sized + 'static> VectorIndex for ReadOnlySnapshot<T> {
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
        match &self.2 {
            Some(control) => self.0.search_knn_with_control(query, k, control),
            None => self.0.search_knn(query, k),
        }
    }

    fn search_threshold(&self, query: &[f32], threshold: f32) -> StorageBackendResult<PostingList> {
        match &self.2 {
            Some(control) => self
                .0
                .search_threshold_with_control(query, threshold, control),
            None => self.0.search_threshold(query, threshold),
        }
    }

    fn search_knn_with_control(
        &self,
        query: &[f32],
        k: usize,
        control: &StorageReadControl,
    ) -> StorageBackendResult<PostingList> {
        control.check()?;
        if self.2.is_some() {
            self.search_knn(query, k)
        } else {
            self.0.search_knn_with_control(query, k, control)
        }
    }

    fn search_threshold_with_control(
        &self,
        query: &[f32],
        threshold: f32,
        control: &StorageReadControl,
    ) -> StorageBackendResult<PostingList> {
        control.check()?;
        if self.2.is_some() {
            self.search_threshold(query, threshold)
        } else {
            self.0
                .search_threshold_with_control(query, threshold, control)
        }
    }

    fn count(&self) -> StorageBackendResult<usize> {
        if let Some(control) = &self.2 {
            control.check()?;
        }
        self.0.count()
    }

    fn contains_document(&self, doc_id: DocId) -> StorageBackendResult<bool> {
        if let Some(control) = &self.2 {
            control.check()?;
        }
        self.0.contains_document(doc_id)
    }

    fn initialize(&mut self) -> StorageBackendResult<()> {
        Err(read_only_error())
    }

    fn snapshot(&self) -> StorageBackendResult<Arc<dyn VectorIndex>> {
        Ok(Arc::new(self.clone()))
    }

    fn vector_read_snapshot(
        &self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<crate::vector_index::VectorReadSnapshot>> {
        control.check()?;
        let original = self.2.as_ref().unwrap_or(control);
        original.check()?;
        self.0
            .vector_read_snapshot(original)?
            .map(|source| {
                let source = ReadOnlySnapshot(source, self.1.clone(), self.2.clone());
                uqa_core::memory::Budgeted::new(source, original.memory().empty_reservation())
                    .into_shared()
                    .map(|source| source as crate::vector_index::VectorReadSnapshot)
                    .map_err(Into::into)
            })
            .transpose()
    }

    fn snapshot_with_vector_read(
        &self,
        source: crate::vector_index::VectorReadSnapshot,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<Arc<dyn VectorIndex>>> {
        control.check()?;
        let original = self.2.as_ref().unwrap_or(control);
        original.check()?;
        Ok(self
            .0
            .snapshot_with_vector_read(source, original)?
            .map(|index| {
                Arc::new(ReadOnlySnapshot(index, self.1.clone(), self.2.clone()))
                    as Arc<dyn VectorIndex>
            }))
    }

    fn diskann_read_snapshot(
        &self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<crate::diskann_index::DiskANNReadSnapshot>> {
        control.check()?;
        if let Some(original) = &self.2 {
            original.check()?;
        }
        self.0
            .diskann_read_snapshot(self.2.as_ref().unwrap_or(control))?
            .map(|source| source.with_guard(self.1.clone(), self.2.clone()))
            .transpose()
    }

    fn snapshot_with_diskann_changes(
        &self,
        changes: &crate::diskann_index::DiskANNReadChanges,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<Arc<dyn VectorIndex>>> {
        control.check()?;
        if let Some(original) = &self.2 {
            original.check()?;
        }
        Ok(self
            .0
            .snapshot_with_diskann_changes(changes, self.2.as_ref().unwrap_or(control))?
            .map(|index| {
                Arc::new(ReadOnlySnapshot(index, self.1.clone(), self.2.clone()))
                    as Arc<dyn VectorIndex>
            }))
    }

    fn snapshot_with_control(
        &self,
        control: &crate::read_control::StorageReadControl,
    ) -> StorageBackendResult<Arc<dyn VectorIndex>> {
        control.check()?;
        if self.1.is_some() || self.2.is_some() {
            return self.clone().with_vector_read_control(control)?.snapshot();
        }
        Ok(Arc::new(ReadOnlySnapshot::new(
            self.0.snapshot_with_control(control)?,
        )))
    }
}

impl<T: crate::vector_index::VectorRead + ?Sized> crate::vector_index::VectorRead
    for ReadOnlySnapshot<T>
{
    fn check_control(&self, control: &StorageReadControl) -> StorageBackendResult<()> {
        if let Some(original) = &self.2 {
            original.check()?;
        }
        self.0.check_control(control)
    }
    fn dimensions(&self) -> u32 {
        self.0.dimensions()
    }
    fn next_document_after(
        &self,
        after: Option<DocId>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DocId>> {
        self.check_control(control)?;
        self.0.next_document_after(after, control)
    }
    fn document_vector_count(
        &self,
        document: DocId,
        control: &StorageReadControl,
    ) -> StorageBackendResult<u64> {
        self.check_control(control)?;
        self.0.document_vector_count(document, control)
    }
    fn read_vector(
        &self,
        document: DocId,
        ordinal: u32,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<uqa_core::memory::BudgetedVec<f32>>> {
        self.check_control(control)?;
        self.0.read_vector(document, ordinal, control)
    }
}
