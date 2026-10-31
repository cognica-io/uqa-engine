//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native planning retains the invoking request's existing allowance and cancellation owner.

use uqa_core::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken,
};

/// Retained request allowance and cancellation inputs shared by statement and retrieval planning.
pub struct PlanningResources {
    memory: MemoryBudget,
    cancellation: CancellationToken,
}

impl PlanningResources {
    pub fn new(memory: MemoryBudget, cancellation: CancellationToken) -> Self {
        Self {
            memory,
            cancellation,
        }
    }

    pub(crate) fn control(&self) -> ProductionControl<'_> {
        ProductionControl::new(&self.memory, &self.cancellation, &self.cancellation)
    }
}
