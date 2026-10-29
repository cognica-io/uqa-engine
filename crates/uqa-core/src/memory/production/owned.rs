//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retained request handles let an owning stateful producer borrow the same allowance and cancellation scopes on each operation.

use super::{MemoryBudget, ProductionControl};
use crate::CancellationToken;

#[derive(Debug, Clone)]
pub struct OwnedProductionControl {
    budget: Option<MemoryBudget>,
    original: Option<CancellationToken>,
    invoking: Option<CancellationToken>,
}

impl ProductionControl<'_> {
    /// Retain existing shared handles without allocating another budget or cancellation scope.
    pub fn capture(&self) -> OwnedProductionControl {
        OwnedProductionControl {
            budget: self.budget.cloned(),
            original: self.original.cloned(),
            invoking: self.invoking.cloned(),
        }
    }
}

impl OwnedProductionControl {
    pub fn control(&self) -> ProductionControl<'_> {
        ProductionControl {
            budget: self.budget.as_ref(),
            original: self.original.as_ref(),
            invoking: self.invoking.as_ref(),
        }
    }
}
