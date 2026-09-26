//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::ops::Bound::{Excluded, Unbounded};
use uqa_core::{
    memory::{Budgeted, BudgetedSharedMap, MemoryError, MemoryReservation},
    DocId,
};

use crate::diskann_index::{
    format::{DiskANNCanonicalOrigin, DiskANNChangeIdentity, DiskANNVectorVersion},
    DiskANNCanonicalRead, DiskANNCanonicalVectorVisitor, DiskANNQueryRead,
};
use crate::{
    read_control::StorageReadControl, vector_index::validate_vector_values_controlled,
    StorageBackendResult,
};

pub(super) struct Tensor {
    origin: DiskANNCanonicalOrigin,
    // Caller-owned input is admitted before this owner adopts its complete capacities.
    values: Budgeted<Vec<Vec<f32>>>,
}

#[derive(Clone)]
pub(super) struct Canonical {
    documents: BudgetedSharedMap<DocId, Tensor>,
    changes: BudgetedSharedMap<DocId, DiskANNVectorVersion>,
    dimensions: u32,
    control: StorageReadControl,
}

impl Canonical {
    pub(super) fn empty(dimensions: u32, control: &StorageReadControl) -> Self {
        Self {
            documents: BudgetedSharedMap::new(control.memory()),
            changes: BudgetedSharedMap::new(control.memory()),
            dimensions,
            control: control.clone(),
        }
    }

    pub(super) fn replaced(
        &self,
        document: DocId,
        values: Vec<Vec<f32>>,
        version: DiskANNVectorVersion,
        mut memory: MemoryReservation,
    ) -> StorageBackendResult<Self> {
        self.control.check()?;
        let count = u64::try_from(values.len()).map_err(|_| MemoryError::SizeOverflow)?;
        let origin = DiskANNCanonicalOrigin::new(version, self.dimensions, count)?;
        let mut bytes = values
            .capacity()
            .checked_mul(size_of::<Vec<f32>>())
            .ok_or(MemoryError::SizeOverflow)?;
        for vector in &values {
            validate_vector_values_controlled(self.dimensions, vector, Some(&self.control))?;
            bytes = bytes
                .checked_add(
                    vector
                        .capacity()
                        .checked_mul(size_of::<f32>())
                        .ok_or(MemoryError::SizeOverflow)?,
                )
                .ok_or(MemoryError::SizeOverflow)?;
        }
        memory.grow(bytes.saturating_sub(memory.bytes()))?;
        let tensor = Tensor {
            origin,
            values: Budgeted::new(values, memory),
        };
        let documents = self.documents.with_insert(document, tensor)?;
        let changes = self.changes.with_insert(document, version)?;
        self.control.check()?;
        Ok(Self {
            documents,
            changes,
            dimensions: self.dimensions,
            control: self.control.clone(),
        })
    }

    pub(super) fn covered(mut self) -> Self {
        self.changes = BudgetedSharedMap::new(self.control.memory());
        self
    }
}

impl DiskANNCanonicalRead for Canonical {
    fn check_control(&self, control: &StorageReadControl) -> StorageBackendResult<()> {
        self.control.check()?;
        control.check()
    }
    fn dimensions(&self) -> u32 {
        self.dimensions
    }
    fn next_document_after(
        &self,
        after: Option<DocId>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DocId>> {
        self.check_control(control)?;
        Ok(self
            .documents
            .range_from(after.as_ref().map_or(Unbounded, Excluded))
            .next()
            .map(|(&document, _)| document))
    }
    fn origin(
        &self,
        document: DocId,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DiskANNVectorVersion>> {
        self.document_origin(document, control)
            .map(|origin| origin.map(DiskANNCanonicalOrigin::version))
    }
    fn visit_document(
        &self,
        document: DocId,
        control: &StorageReadControl,
        visit: &mut DiskANNCanonicalVectorVisitor<'_>,
    ) -> StorageBackendResult<Option<DiskANNVectorVersion>> {
        self.check_control(control)?;
        let Some(tensor) = self.documents.get(&document) else {
            return Ok(None);
        };
        for (ordinal, raw) in tensor.values.iter().enumerate() {
            self.check_control(control)?;
            visit(ordinal as u32, tensor.origin.version(), raw)?;
        }
        self.check_control(control)?;
        Ok(Some(tensor.origin.version()))
    }
}

impl DiskANNQueryRead for Canonical {
    fn document_origin(
        &self,
        document: DocId,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DiskANNCanonicalOrigin>> {
        self.check_control(control)?;
        Ok(self.documents.get(&document).map(|tensor| tensor.origin))
    }
    fn next_change_after(
        &self,
        after: Option<DocId>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DiskANNChangeIdentity>> {
        self.check_control(control)?;
        Ok(self
            .changes
            .range_from(after.as_ref().map_or(Unbounded, Excluded))
            .next()
            .map(|(&document, &version)| DiskANNChangeIdentity::new(document, version)))
    }
}
