//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrowed observation of the executable's live, directly owned storage.

use super::{CommandPlan, UnifiedPlan};
use crate::{result::explain::ExplainPlanningMemory, SQLError};
use uqa_core::memory::{
    AllocationObserver, AllocationUsage, ObserveAllocations, ProductionControl,
};
use uqa_core::ValueRetentionError;

mod assignments;
mod command;
mod metadata;
mod query;
mod scalar;
mod source;
#[cfg(test)]
mod tests;

impl UnifiedPlan {
    /// Observe initialized bytes and exposed allocation capacity before execution. Shared literal owners are counted once without decoding. Immutable analysis/catalog descriptors, opaque dependency storage and allocator bookkeeping remain outside the executable's storage. This neither evaluates expressions nor reserves the already-produced plan again.
    pub fn planning_memory(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<ExplainPlanningMemory, SQLError> {
        let usage = AllocationUsage::measure(self, control)?;
        Ok(ExplainPlanningMemory {
            used_bytes: usage.used_bytes,
            allocated_bytes: usage.allocated_bytes,
        })
    }
}

impl ObserveAllocations for CommandPlan {
    fn observe_allocations<'a>(
        &'a self,
        observer: &mut AllocationObserver<'a, '_>,
    ) -> Result<(), ValueRetentionError> {
        match self {
            Self::Insert(plan) => observer.child(plan),
            Self::Update(plan) => observer.child(plan),
            Self::Delete(plan) => observer.child(plan),
            Self::Merge(plan) => observer.child(plan),
            _ => Err(ValueRetentionError::Malformed {
                kind: "executable plan",
                reason: "utility command has a separate planning boundary".into(),
            }),
        }
    }
}

impl ObserveAllocations for crate::catalog::roles::RoleReference {
    fn observe_allocations<'a>(
        &'a self,
        observer: &mut AllocationObserver<'a, '_>,
    ) -> Result<(), ValueRetentionError> {
        match self {
            Self::Named(name) => observer.child(name),
            // The immutable catalog binding retains its own allocation owner.
            Self::Bound(_) => Ok(()),
        }
    }
}
