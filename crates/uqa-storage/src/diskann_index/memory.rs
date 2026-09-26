//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Mutable memory roots share immutable pages and copy only changed canonical paths.

mod build;
mod source;
#[cfg(test)]
mod tests;

use super::{
    build::DiskANNTemporaryBudget,
    format::{DiskANNGeneration, DiskANNManifest, DiskANNVectorVersion},
    RetainedDiskANNIndex,
};
use crate::{
    mvcc::{DatabaseId, StorageTransactionId},
    read_control::StorageReadControl,
    StorageBackendError, StorageBackendResult, VectorIndex,
};
use source::Canonical;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use uqa_core::{
    memory::{Budgeted, MemoryReservation},
    DocId, PostingList,
};

/// Memory construction uses the same host settings as persistent generation construction.
pub type DiskANNMemoryOptions = super::DiskANNIndexOptions;

struct Clock {
    database: DatabaseId,
    next: AtomicU64,
}

impl Clock {
    fn allocate(&self) -> StorageBackendResult<u64> {
        self.next
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |next| {
                next.checked_add(1)
            })
            .map_err(|_| StorageBackendError::Other("DiskANN memory identity exhausted".into()))
    }
    fn generation(&self) -> StorageBackendResult<DiskANNGeneration> {
        DiskANNGeneration::new(self.database.as_bytes(), 1, 1, self.allocate()?)
    }
    fn version(&self) -> StorageBackendResult<DiskANNVectorVersion> {
        // This incarnation owns one mutable memory lineage. Every fork shares its non-reused revision allocator; the identity is not SQL commit order.
        let writer = StorageTransactionId::new(self.database, 1)
            .map_err(crate::mvcc::VersionError::into_storage_error)?;
        DiskANNVectorVersion::new(writer, self.allocate()?)
    }
}

/// A writable memory index over the same sealed-page search as persistent providers. Ordinary mutations replace canonical tensors and current changes atomically; explicit initialization and clear construct replacement physical generations. Writable snapshots retain independent roots under the same original allowance.
pub struct DiskANNMemoryIndex {
    index: RetainedDiskANNIndex<Canonical>,
    clock: Arc<Budgeted<Clock>>,
    options: DiskANNMemoryOptions,
    temporary: DiskANNTemporaryBudget,
    control: StorageReadControl,
    _memory: MemoryReservation,
}

impl DiskANNMemoryIndex {
    pub fn new(
        dimensions: u32,
        options: DiskANNMemoryOptions,
        temporary: &DiskANNTemporaryBudget,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        options.parameters.validate(dimensions)?;
        let memory = control.memory().reserve(size_of::<Self>())?;
        let database = DatabaseId::from_bytes(crate::catalog::new_nonzero_catalog_identity(
            "DiskANN memory",
            "incarnation",
        )?);
        let clock = Budgeted::new(
            Clock {
                database,
                next: AtomicU64::new(1),
            },
            control.memory().empty_reservation(),
        )
        .into_shared()?;
        let index = build::prepare(
            Canonical::empty(dimensions, control),
            clock.generation()?,
            options,
            temporary,
            control,
        )?;
        Ok(Self {
            index,
            clock,
            options,
            temporary: temporary.clone(),
            control: control.clone(),
            _memory: memory,
        })
    }

    pub fn manifest(&self) -> &DiskANNManifest {
        self.index.manifest()
    }

    fn fork(&self) -> StorageBackendResult<Self> {
        self.control.check()?;
        let memory = self.control.memory().reserve(size_of::<Self>())?;
        Ok(Self {
            index: self.index.clone(),
            clock: self.clock.clone(),
            options: self.options,
            temporary: self.temporary.clone(),
            control: self.control.clone(),
            _memory: memory,
        })
    }

    fn replace(
        &mut self,
        document: DocId,
        values: Vec<Vec<f32>>,
        memory: MemoryReservation,
    ) -> StorageBackendResult<()> {
        self.control.check()?;
        let canonical =
            self.index
                .canonical()
                .replaced(document, values, self.clock.version()?, memory)?;
        let candidate = self.index.with_canonical(canonical)?;
        self.control.check()?;
        self.index = candidate;
        Ok(())
    }

    fn rebuild(&mut self, canonical: Canonical) -> StorageBackendResult<()> {
        self.control.check()?;
        let candidate = build::prepare(
            canonical,
            self.clock.generation()?,
            self.options,
            &self.temporary,
            &self.control,
        )?;
        self.control.check()?;
        self.index = candidate;
        Ok(())
    }
}

impl VectorIndex for DiskANNMemoryIndex {
    fn dimensions(&self) -> u32 {
        self.index.dimensions()
    }
    fn index_kind(&self) -> &'static str {
        "diskann"
    }
    fn add(&mut self, document: DocId, vector: Vec<f32>) -> StorageBackendResult<()> {
        // Admit the one-element tensor header before allocating this adapter-owned wrapper; coordinates remain caller-owned until replacement adopts them.
        self.control.check()?;
        let header = self.control.memory().reserve(size_of::<Vec<f32>>())?;
        let values = vec![vector];
        self.replace(document, values, header)
    }
    fn add_many(&mut self, document: DocId, vectors: Vec<Vec<f32>>) -> StorageBackendResult<()> {
        self.replace(document, vectors, self.control.memory().empty_reservation())
    }
    fn delete(&mut self, document: DocId) -> StorageBackendResult<()> {
        if !self.index.contains_document(document)? {
            return Ok(());
        }
        self.replace(
            document,
            Vec::new(),
            self.control.memory().empty_reservation(),
        )
    }
    fn clear(&mut self) -> StorageBackendResult<()> {
        self.rebuild(Canonical::empty(self.dimensions(), &self.control))
    }
    fn initialize(&mut self) -> StorageBackendResult<()> {
        self.rebuild(self.index.canonical().clone())
    }
    fn search_knn(&self, query: &[f32], k: usize) -> StorageBackendResult<PostingList> {
        self.index.search_knn(query, k)
    }
    fn search_threshold(&self, query: &[f32], threshold: f32) -> StorageBackendResult<PostingList> {
        self.index.search_threshold(query, threshold)
    }
    fn search_knn_with_control(
        &self,
        query: &[f32],
        k: usize,
        control: &StorageReadControl,
    ) -> StorageBackendResult<PostingList> {
        self.index.search_knn_with_control(query, k, control)
    }
    fn search_threshold_with_control(
        &self,
        query: &[f32],
        threshold: f32,
        control: &StorageReadControl,
    ) -> StorageBackendResult<PostingList> {
        self.index
            .search_threshold_with_control(query, threshold, control)
    }
    fn count(&self) -> StorageBackendResult<usize> {
        self.index.count()
    }
    fn contains_document(&self, document: DocId) -> StorageBackendResult<bool> {
        self.index.contains_document(document)
    }
    fn snapshot(&self) -> StorageBackendResult<Arc<dyn VectorIndex>> {
        self.index.snapshot()
    }
    fn snapshot_with_control(
        &self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Arc<dyn VectorIndex>> {
        self.index.snapshot_with_control(control)
    }
    fn writable_snapshot(&self) -> StorageBackendResult<Box<dyn VectorIndex>> {
        Ok(Box::new(self.fork()?))
    }
}
