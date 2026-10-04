//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Canonical reads seek active identities without materializing a document directory.

use super::{types::active_key, HNSWIndex};
use crate::{
    read_control::StorageReadControl,
    vector_index::{copy_vector, VectorRead},
    StorageBackendResult,
};
use uqa_core::{memory::BudgetedVec, DocId};

impl VectorRead for HNSWIndex {
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
            .active
            .next(after.map(|doc| active_key(doc, u32::MAX)))?
            .map(|(key, _)| (key >> 32) as u64))
    }

    fn document_vector_count(
        &self,
        document: DocId,
        control: &StorageReadControl,
    ) -> StorageBackendResult<u64> {
        let mut after = active_key(document, 0).checked_sub(1);
        let mut count = 0;
        while let Some((key, _)) = self.active.next(after)? {
            control.check()?;
            if key >> 32 != u128::from(document) {
                break;
            }
            if u64::from(key as u32) != count {
                return Err(crate::mvcc::VersionError::InvalidEncoding(
                    "HNSW live document vector ordinals are not contiguous",
                )
                .into_storage_error());
            }
            count += 1;
            after = Some(key);
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
        let Some(node_id) = self.active.get(active_key(document, ordinal))? else {
            return Ok(None);
        };
        let node = self
            .node(*node_id)?
            .filter(|node| {
                !node.deleted && node.doc_id == document && node.vector_ordinal == ordinal
            })
            .ok_or_else(|| {
                crate::mvcc::VersionError::InvalidEncoding(
                    "HNSW canonical identity has no active raw vector",
                )
                .into_storage_error()
            })?;
        copy_vector(&node.raw_vector, control).map(Some)
    }
}
