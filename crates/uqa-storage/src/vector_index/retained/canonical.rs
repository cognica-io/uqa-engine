//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrow the existing retained corpus without copying its directory or coordinates at capture.

use super::RetainedVectorIndex;
use crate::{read_control::StorageReadControl, vector_index::VectorRead, StorageBackendResult};
use uqa_core::{memory::BudgetedVec, DocId};

impl VectorRead for RetainedVectorIndex {
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
        let offset = self
            .entries
            .partition_point(|entry| after.is_some_and(|after| entry.0 <= after));
        Ok(self.entries.get(offset).map(|entry| entry.0))
    }
    fn document_vector_count(
        &self,
        document: DocId,
        control: &StorageReadControl,
    ) -> StorageBackendResult<u64> {
        self.check_control(control)?;
        let start = self.entries.partition_point(|entry| entry.0 < document);
        let end = self.entries.partition_point(|entry| entry.0 <= document);
        Ok((end - start) as u64)
    }
    fn read_vector(
        &self,
        document: DocId,
        ordinal: u32,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<BudgetedVec<f32>>> {
        self.check_control(control)?;
        let Ok(position) = self
            .entries
            .binary_search_by_key(&(document, ordinal), |entry| (entry.0, entry.1))
        else {
            return Ok(None);
        };
        let mut vector = BudgetedVec::new(control.memory());
        vector.reserve(self.entries[position].2.len())?;
        for chunk in self.entries[position].2.chunks(1024) {
            self.check_control(control)?;
            vector.extend_from_slice(chunk)?;
        }
        self.check_control(control)?;
        Ok(Some(vector))
    }
}
