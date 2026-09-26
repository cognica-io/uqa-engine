//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Opaque canonical capabilities retain actual provider ownership and every enclosing read guard.

use crate::diskann_index::{
    format::{
        DiskANNCanonicalOrigin, DiskANNChangeIdentity, DiskANNGeneration, DiskANNVectorVersion,
    },
    DiskANNCanonicalRead, DiskANNCanonicalVectorVisitor, DiskANNQueryRead,
};
use crate::{read_control::StorageReadControl, StorageBackendResult};
use std::sync::Arc;
use uqa_core::{
    memory::{Budgeted, MemoryReservation},
    DocId,
};

/// An actual selected `DiskANN` canonical source. Only storage snapshot owners construct this capability; callers cannot substitute a generation label or fabricate vector origins. Clones share canonical rows, prepared ownership and read controls.
#[derive(Clone)]
pub struct DiskANNReadSnapshot {
    canonical: Arc<dyn DiskANNQueryRead + Send + Sync>,
    generation: DiskANNGeneration,
    control: StorageReadControl,
    guard: Option<Arc<Budgeted<Guard>>>,
}

struct Guard {
    previous: Option<Arc<Budgeted<Guard>>>,
    control: Option<StorageReadControl>,
    _memory: Option<Arc<MemoryReservation>>,
}

impl DiskANNReadSnapshot {
    pub(in crate::diskann_index::query) fn new(
        canonical: Arc<dyn DiskANNQueryRead + Send + Sync>,
        generation: DiskANNGeneration,
        control: &StorageReadControl,
    ) -> Self {
        Self {
            canonical,
            generation,
            control: control.clone(),
            guard: None,
        }
    }

    /// Whether both actual sources belong to the same database, table and index incarnation. This metadata comparison does not establish equal visibility, generation, configuration or canonical width.
    pub fn same_lineage(&self, other: &Self) -> bool {
        self.generation.database() == other.generation.database()
            && self.generation.table() == other.generation.table()
            && self.generation.index() == other.generation.index()
    }

    pub(crate) fn with_guard(
        mut self,
        memory: Option<Arc<MemoryReservation>>,
        control: Option<StorageReadControl>,
    ) -> StorageBackendResult<Self> {
        self.check_control(&self.control)?;
        if let Some(control) = &control {
            control.check()?;
        }
        if memory.is_some() || control.is_some() {
            let guard = Guard {
                previous: self.guard.take(),
                control,
                _memory: memory,
            };
            self.guard = Some(
                Budgeted::new(guard, self.control.memory().empty_reservation()).into_shared()?,
            );
        }
        Ok(self)
    }

    fn read<T>(
        &self,
        control: &StorageReadControl,
        operation: impl FnOnce() -> StorageBackendResult<T>,
    ) -> StorageBackendResult<T> {
        self.check_control(control)?;
        let result = operation()?;
        self.check_control(control)?;
        Ok(result)
    }
}

impl DiskANNCanonicalRead for DiskANNReadSnapshot {
    fn check_control(&self, control: &StorageReadControl) -> StorageBackendResult<()> {
        self.control.check()?;
        control.check()?;
        let mut guard = self.guard.as_ref();
        while let Some(current) = guard {
            if let Some(control) = &current.control {
                control.check()?;
            }
            guard = current.previous.as_ref();
        }
        self.canonical.check_control(control)
    }

    fn dimensions(&self) -> u32 {
        self.canonical.dimensions()
    }

    fn next_document_after(
        &self,
        after: Option<DocId>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DocId>> {
        self.read(control, || {
            self.canonical.next_document_after(after, control)
        })
    }

    fn origin(
        &self,
        document: DocId,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DiskANNVectorVersion>> {
        self.read(control, || self.canonical.origin(document, control))
    }

    fn visit_document(
        &self,
        document: DocId,
        control: &StorageReadControl,
        visit: &mut DiskANNCanonicalVectorVisitor<'_>,
    ) -> StorageBackendResult<Option<DiskANNVectorVersion>> {
        self.read(control, || {
            self.canonical.visit_document(document, control, visit)
        })
    }
}

impl DiskANNQueryRead for DiskANNReadSnapshot {
    fn document_origin(
        &self,
        document: DocId,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DiskANNCanonicalOrigin>> {
        self.read(control, || {
            self.canonical.document_origin(document, control)
        })
    }

    fn next_change_after(
        &self,
        after: Option<DocId>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DiskANNChangeIdentity>> {
        self.read(control, || self.canonical.next_change_after(after, control))
    }
}
