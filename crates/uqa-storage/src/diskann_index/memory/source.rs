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
    format::{
        DiskANNCanonicalOrigin, DiskANNChangeIdentity, DiskANNGeneration, DiskANNVectorVersion,
    },
    DiskANNCanonicalCounts, DiskANNCanonicalRead, DiskANNCanonicalVectorVisitor, DiskANNQueryRead,
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
    counts: DiskANNCanonicalCounts,
    covered_generation: Option<DiskANNGeneration>,
    dimensions: u32,
    control: StorageReadControl,
    revision: Option<DiskANNVectorVersion>,
}

impl Canonical {
    pub(super) fn empty(dimensions: u32, control: &StorageReadControl) -> Self {
        Self {
            documents: BudgetedSharedMap::new(control.memory()),
            changes: BudgetedSharedMap::new(control.memory()),
            counts: DiskANNCanonicalCounts::default(),
            covered_generation: None,
            dimensions,
            control: control.clone(),
            revision: None,
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
        let counts = self.counts.replaced(
            self.documents
                .get(&document)
                .map_or(0, |tensor| tensor.origin.count()),
            self.changes.get(&document).is_some(),
            count,
        )?;
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
            counts,
            covered_generation: self.covered_generation,
            dimensions: self.dimensions,
            control: self.control.clone(),
            revision: Some(version),
        })
    }

    pub(super) fn covered(mut self, generation: DiskANNGeneration) -> Self {
        self.changes = BudgetedSharedMap::new(self.control.memory());
        self.counts = self.counts.covered();
        self.covered_generation = Some(generation);
        self
    }
}

impl DiskANNCanonicalRead for Canonical {
    fn population_counts(
        &self,
        generation: DiskANNGeneration,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DiskANNCanonicalCounts>> {
        self.check_control(control)?;
        Ok((self.covered_generation == Some(generation)).then_some(self.counts))
    }

    fn corpus_fingerprint(
        &self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<[u8; 32]>> {
        use sha2::{Digest, Sha256};
        self.check_control(control)?;
        let mut digest = Sha256::new();
        digest.update(b"uqa-memory-diskann-corpus-v1\0");
        digest.update(self.dimensions.to_le_bytes());
        if let Some(revision) = self.revision {
            digest.update([1]);
            digest.update(revision.writer().database().as_bytes());
            digest.update(revision.writer().allocation().to_le_bytes());
            digest.update(revision.revision().to_le_bytes());
        } else {
            digest.update([0]);
        }
        Ok(Some(digest.finalize().into()))
    }
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
    fn read_vector(
        &self,
        document: DocId,
        ordinal: u32,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<uqa_core::memory::BudgetedVec<f32>>> {
        self.check_control(control)?;
        let Some(raw) = self
            .documents
            .get(&document)
            .and_then(|tensor| tensor.values.get(ordinal as usize))
        else {
            return Ok(None);
        };
        let mut vector = uqa_core::memory::BudgetedVec::new(control.memory());
        vector.reserve(raw.len())?;
        for chunk in raw.chunks(1024) {
            self.check_control(control)?;
            vector.extend_from_slice(chunk)?;
        }
        Ok(Some(vector))
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
