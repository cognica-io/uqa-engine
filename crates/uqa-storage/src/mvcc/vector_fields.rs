//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical layouts identify field coordination records; common MVCC owns their retirement.

use uqa_core::memory::BudgetedVec;

use crate::read_control::StorageReadControl;

use super::VersionResult;

/// The two coordination keys and their canonical vector domain. Buffers retain the invoking maintenance allowance.
pub struct VectorFieldGuard {
    pub lifetime: BudgetedVec<u8>,
    pub references: BudgetedVec<u8>,
    pub reference_value: BudgetedVec<u8>,
    pub vectors: BudgetedVec<u8>,
}

/// Decode only this provider's internal vector-field guard namespace. A lifetime key returns `None`; a reference key supplies its exact immutable value and paired lifetime. Malformed keys reject, and neighboring metadata is outside `prefix`.
pub trait VectorFieldGuardLayout: Send + Sync {
    fn prefix(&self, control: &StorageReadControl) -> VersionResult<BudgetedVec<u8>>;

    fn reference(
        &self,
        key: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<Option<VectorFieldGuard>>;
}
