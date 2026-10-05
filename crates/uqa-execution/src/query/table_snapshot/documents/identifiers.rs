//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Query-owned identity pages keep their merge buffers charged through internal consumers.

use super::{DocId, RetainedDocuments, StorageBackendResult};
use uqa_core::memory::BudgetedVec;
use uqa_storage::read_control::StorageReadControl;

impl RetainedDocuments {
    pub(in crate::query::table_snapshot) fn id_page(
        &self,
        after: Option<DocId>,
        limit: usize,
    ) -> StorageBackendResult<BudgetedVec<DocId>> {
        self.merged_id_page(after, limit, &self.0.control, true)
    }

    pub(super) fn id_page_controlled(
        &self,
        after: Option<DocId>,
        limit: usize,
        control: &StorageReadControl,
    ) -> StorageBackendResult<BudgetedVec<DocId>> {
        self.merged_id_page(after, limit, control, false)
    }

    fn merged_id_page(
        &self,
        after: Option<DocId>,
        limit: usize,
        control: &StorageReadControl,
        borrowed: bool,
    ) -> StorageBackendResult<BudgetedVec<DocId>> {
        crate::query::document_changes::VisibleDocumentIds {
            source: self.0.source.as_ref(),
            changes: &self.0.changes,
            control: &self.0.control,
        }
        .page(after, limit, control, borrowed)
    }
}
