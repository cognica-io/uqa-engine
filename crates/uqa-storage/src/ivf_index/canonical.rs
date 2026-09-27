//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Raw tensor reads remain independent of training, centroids and probe selection.

use super::IVFIndex;
use crate::{
    read_control::StorageReadControl,
    vector_index::{copy_vector, ordinal_count, VectorRead},
    StorageBackendResult,
};
use std::ops::Bound::{Excluded, Unbounded};
use uqa_core::{memory::BudgetedVec, DocId};

impl VectorRead for IVFIndex {
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
            .lock()
            .range((
                after.map_or(Unbounded, |document| Excluded((document, u32::MAX))),
                Unbounded,
            ))
            .next()
            .map(|(key, _)| key.0))
    }
    fn document_vector_count(
        &self,
        document: DocId,
        control: &StorageReadControl,
    ) -> StorageBackendResult<u64> {
        ordinal_count(
            self.vectors
                .lock()
                .range((document, 0)..=(document, u32::MAX))
                .map(|(key, _)| key.1),
            control,
        )
    }
    fn read_vector(
        &self,
        document: DocId,
        ordinal: u32,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<BudgetedVec<f32>>> {
        control.check()?;
        let vectors = self.vectors.lock();
        vectors
            .get(&(document, ordinal))
            .map(|vector| copy_vector(&vector.raw_vector, control))
            .transpose()
    }
}
