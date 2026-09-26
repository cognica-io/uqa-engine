//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Runtime vector operations share the provider's original catalog and transaction owner.

use super::{build::DiskANNTemporaryBudget, pages::DiskANNReadLimits, DiskANNIndexOptions};
use crate::{
    read_control::StorageReadControl, vector_index::DiskANNIndexParams, StorageBackendError,
    StorageBackendResult, VectorIndex,
};
use std::sync::Arc;
use uqa_core::{memory::MemoryReservation, DocId, PostingList};

/// Actual SQL catalog address and the caller's retained allowance. Providers resolve immutable ownership from the stored definition through the supplied SQL owner.
pub struct DiskANNIndexBinding<'a> {
    pub table: &'a str,
    pub field: &'a str,
    pub dimensions: u32,
    pub index: &'a crate::RelationIdentity,
    pub resolver: Arc<dyn super::catalog::DiskANNIndexResolver + Send + Sync>,
    pub control: &'a StorageReadControl,
}

/// Provider boundary for a bound mutable index. Structural operations preserve the active caller transaction and restore their own private effects on failure.
pub trait DiskANNPersistentOwner: Send + Sync {
    type Snapshot: VectorIndex + 'static;
    fn dimensions(&self) -> u32;
    fn parameters(&self) -> DiskANNIndexParams;
    fn read_limits(&self) -> DiskANNReadLimits;
    fn control(&self) -> &StorageReadControl;
    fn snapshot(&self) -> StorageBackendResult<Self::Snapshot>;
    fn count(&self) -> StorageBackendResult<usize>;
    fn contains_document(&self, document: DocId) -> StorageBackendResult<bool>;
    fn replace(&self, document: DocId, vectors: &[Vec<f32>]) -> StorageBackendResult<()>;
    fn rebuild(
        &self,
        options: DiskANNIndexOptions,
        temporary: &DiskANNTemporaryBudget,
        clear: bool,
    ) -> StorageBackendResult<()>;
}

/// A mutable persistent `VectorIndex`: ordinary writes preserve the base; explicit initialize/clear build and publish replacement generations through the same bound provider session.
pub struct PersistentDiskANNIndex<P> {
    owner: P,
    options: DiskANNIndexOptions,
    temporary: DiskANNTemporaryBudget,
    _memory: MemoryReservation,
}

impl<P: DiskANNPersistentOwner> PersistentDiskANNIndex<P> {
    pub fn new(
        owner: P,
        options: DiskANNIndexOptions,
        temporary: &DiskANNTemporaryBudget,
    ) -> StorageBackendResult<Self> {
        owner.control().check()?;
        if options.parameters != owner.parameters() || options.read != owner.read_limits() {
            return Err(StorageBackendError::Other(
                "DiskANN runtime options differ from its bound catalog or read limits".into(),
            ));
        }
        options.parameters.validate(owner.dimensions())?;
        let memory = owner.control().memory().reserve(size_of::<Self>())?;
        // Restoration validates actual resident data under the original allowance before exposing a live runtime handle. It never rebuilds missing or corrupt state.
        drop(owner.snapshot()?);
        Ok(Self {
            owner,
            options,
            temporary: temporary.clone(),
            _memory: memory,
        })
    }
}

impl<P: DiskANNPersistentOwner> VectorIndex for PersistentDiskANNIndex<P> {
    fn dimensions(&self) -> u32 {
        self.owner.dimensions()
    }
    fn index_kind(&self) -> &'static str {
        "diskann"
    }
    fn add(&mut self, document: DocId, vector: Vec<f32>) -> StorageBackendResult<()> {
        self.owner.replace(document, std::slice::from_ref(&vector))
    }
    fn add_many(&mut self, document: DocId, vectors: Vec<Vec<f32>>) -> StorageBackendResult<()> {
        self.owner.replace(document, &vectors)
    }
    fn delete(&mut self, document: DocId) -> StorageBackendResult<()> {
        self.owner.replace(document, &[])
    }
    fn clear(&mut self) -> StorageBackendResult<()> {
        self.owner.rebuild(self.options, &self.temporary, true)
    }
    fn initialize(&mut self) -> StorageBackendResult<()> {
        self.owner.rebuild(self.options, &self.temporary, false)
    }
    fn search_knn(&self, query: &[f32], k: usize) -> StorageBackendResult<PostingList> {
        self.owner.snapshot()?.search_knn(query, k)
    }
    fn search_threshold(&self, query: &[f32], threshold: f32) -> StorageBackendResult<PostingList> {
        self.owner.snapshot()?.search_threshold(query, threshold)
    }
    fn search_knn_with_control(
        &self,
        query: &[f32],
        k: usize,
        control: &StorageReadControl,
    ) -> StorageBackendResult<PostingList> {
        control.check()?;
        self.owner
            .snapshot()?
            .search_knn_with_control(query, k, control)
    }
    fn search_threshold_with_control(
        &self,
        query: &[f32],
        threshold: f32,
        control: &StorageReadControl,
    ) -> StorageBackendResult<PostingList> {
        control.check()?;
        self.owner
            .snapshot()?
            .search_threshold_with_control(query, threshold, control)
    }
    fn count(&self) -> StorageBackendResult<usize> {
        self.owner.count()
    }
    fn contains_document(&self, document: DocId) -> StorageBackendResult<bool> {
        self.owner.contains_document(document)
    }
    fn snapshot(&self) -> StorageBackendResult<Arc<dyn VectorIndex>> {
        self.owner.snapshot()?.snapshot()
    }
    fn snapshot_with_control(
        &self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Arc<dyn VectorIndex>> {
        control.check()?;
        self.owner.snapshot()?.snapshot_with_control(control)
    }
}
