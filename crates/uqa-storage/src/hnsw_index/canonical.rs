//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Canonical reads use active raw vectors, independently of graph navigation and normalization.

use super::HNSWIndex;
use crate::{
    read_control::StorageReadControl,
    vector_index::{copy_vector, ordinal_count, VectorRead},
    StorageBackendResult,
};
use std::ops::Bound::{Excluded, Unbounded};
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
            self.active
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
        let Some(node) = self.active.get(&(document, ordinal)) else {
            return Ok(None);
        };
        let node = self
            .nodes
            .get(node)
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
