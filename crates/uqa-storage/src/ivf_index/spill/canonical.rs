//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Canonical creation preserves complete-document training boundaries without collecting tensors.

use super::{corrupt, key, IVFMetadataSnapshot, IVFPreparedMetadata, IVFRestoreBuilder, IVFState};
use crate::{
    read_control::StorageReadControl, vector_index::VectorRead, IVFIndexParams,
    StorageBackendResult,
};
use uqa_core::{
    memory::{BudgetedVec, MemoryError},
    DocId,
};

pub(crate) struct IVFCanonicalBuilder {
    candidate: IVFPreparedMetadata,
    previous: Option<(DocId, u32)>,
}

impl IVFCanonicalBuilder {
    pub(crate) fn new(
        dimensions: u32,
        params: IVFIndexParams,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        let header = IVFMetadataSnapshot {
            state: IVFState::Untrained,
            centroids: Vec::new(),
            assignments: Vec::new(),
            trained_size: 0,
            deletes_since_train: 0,
            vector_count: 0,
        };
        Ok(Self {
            candidate: IVFRestoreBuilder::new(dimensions, params, header, control)?.finish()?,
            previous: None,
        })
    }

    pub(crate) fn vector(
        &mut self,
        document: DocId,
        ordinal: u32,
        raw: &[f32],
    ) -> StorageBackendResult<()> {
        self.candidate.control.check()?;
        let expected = match self.previous {
            Some((prior, order)) if prior == document => u64::from(order) + 1,
            Some((prior, _)) if prior > document => {
                return Err(corrupt("unordered canonical vectors"))
            }
            _ => {
                self.complete_document()?;
                0
            }
        };
        if u64::from(ordinal) != expected {
            return Err(corrupt("canonical tensor has non-contiguous ordinals"));
        }
        crate::vector_index::validate_vector_values(self.candidate.dimensions, raw)?;
        self.candidate.insert_assigned(document, ordinal, raw)?;
        self.previous = Some((document, ordinal));
        Ok(())
    }

    fn complete_document(&mut self) -> StorageBackendResult<()> {
        self.candidate.snapshot.vector_count = self.candidate.vectors.len();
        if self.candidate.snapshot.state == IVFState::Untrained
            && self.candidate.vectors.len() >= self.candidate.params.train_threshold
        {
            self.candidate.train()?;
        }
        Ok(())
    }

    pub(crate) fn finish(mut self) -> StorageBackendResult<IVFPreparedMetadata> {
        self.complete_document()?;
        self.candidate.control.check()?;
        Ok(self.candidate)
    }
}

impl IVFPreparedMetadata {
    /// Clone an immutable generation under the requested control, sharing roots only when the memory allowance is unchanged.
    pub fn clone_controlled(&self, control: &StorageReadControl) -> StorageBackendResult<Self> {
        control.check()?;
        let bytes = self.snapshot.centroids.iter().try_fold(
            self.snapshot
                .centroids
                .len()
                .checked_mul(size_of::<Vec<f32>>())
                .ok_or(MemoryError::SizeOverflow)?,
            |bytes, vector| {
                vector
                    .len()
                    .checked_mul(4)
                    .and_then(|count| bytes.checked_add(count))
                    .ok_or(MemoryError::SizeOverflow)
            },
        )?;
        let memory = control.memory().reserve(bytes)?;
        let mut snapshot = self.snapshot.clone();
        snapshot.assignments = Vec::new();
        let vectors = if self.control.memory().shares_allowance(control.memory()) {
            self.vectors.clone()
        } else {
            let mut copied = super::Map::new(control.memory(), control.memory().limit() / 3);
            let mut after = None;
            while let Some((key, vector)) =
                self.vectors.next_with_memory(after, control.memory())?
            {
                control.check()?;
                let _memory = control
                    .memory()
                    .reserve(crate::spill_map::Record::memory_bytes(&*vector)?)?;
                copied.insert(key, (*vector).clone(), Some(control))?;
                after = Some(key);
            }
            copied
        };
        Ok(Self {
            snapshot,
            vectors,
            params: self.params,
            dimensions: self.dimensions,
            control: control.clone(),
            memory,
        })
    }
}

impl VectorRead for IVFPreparedMetadata {
    fn check_control(&self, control: &StorageReadControl) -> StorageBackendResult<()> {
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
        control.check()?;
        Ok(self
            .vectors
            .next_key(
                after.map(|document| key(document, u32::MAX)),
                control.memory(),
            )?
            .map(|key| (key >> 32) as DocId))
    }
    fn document_vector_count(
        &self,
        document: DocId,
        control: &StorageReadControl,
    ) -> StorageBackendResult<u64> {
        let mut after = key(document, 0).checked_sub(1);
        let mut count = 0_u64;
        loop {
            control.check()?;
            let Some(found) = self.vectors.next_key(after, control.memory())? else {
                break;
            };
            if found >> 32 != u128::from(document) {
                break;
            }
            if u64::from(found as u32) != count {
                return Err(corrupt("canonical tensor has non-contiguous ordinals"));
            }
            count += 1;
            after = Some(found);
        }
        Ok(count)
    }
    fn read_vector(
        &self,
        document: DocId,
        ordinal: u32,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<BudgetedVec<f32>>> {
        control.check()?;
        self.vectors
            .get_with_memory(key(document, ordinal), control.memory())?
            .map(|vector| crate::vector_index::copy_vector(&vector.raw_vector, control))
            .transpose()
    }
}
