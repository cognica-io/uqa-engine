//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Field transitions fence vector writers while ordinary document writers remain mergeable.

use super::{KeyValueBatch, KeyValueVectorIndex, StorageBackendResult};
use uqa_core::memory::{BudgetedVec, MemoryError};

const ROOT: &[u8] = b"\0uqa-vector-field-guards-v1\0";

impl KeyValueVectorIndex {
    pub(super) fn coordinate_field(
        &self,
        batch: &mut dyn KeyValueBatch,
        structural: bool,
    ) -> StorageBackendResult<()> {
        if !self.store.transaction_model().is_versioned() {
            return Ok(());
        }
        let control = self.store.retention_control().ok_or_else(|| {
            super::other_error("versioned vector owner has no retention allowance")
        })?;
        control.check()?;
        let bytes = self
            .table
            .len()
            .checked_add(self.field.len())
            .and_then(|size| size.checked_add(32))
            .and_then(|size| size.checked_mul(2))
            .ok_or(MemoryError::SizeOverflow)?;
        let _workspace = control.memory().reserve(bytes)?;
        let prefix = super::vector_field_prefix(&self.table, &self.field)?;
        let mut lifetime = BudgetedVec::new(control.memory());
        lifetime.extend_from_slice(ROOT)?;
        lifetime.push(0)?;
        lifetime.extend_from_slice(&prefix)?;
        let mut references = BudgetedVec::new(control.memory());
        references.extend_from_slice(ROOT)?;
        references.push(1)?;
        references.extend_from_slice(&prefix)?;
        if structural {
            batch.fence_record(&lifetime)?;
            batch.fence_record(&references)
        } else {
            batch.require_unchanged(&lifetime)?;
            batch.touch_marker(&references, &[1])
        }
    }
}
