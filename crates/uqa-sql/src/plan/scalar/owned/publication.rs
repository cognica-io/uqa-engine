//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Frozen scalars keep iterative retirement until their final fallible publication completes.

use super::{
    MemoryReservation, OwnedScalarExpr, Produced, ProductionControl, Result, ScalarExpr,
    ScalarMemory,
};

pub(super) struct Publication {
    // The scalar retires before the folded lease on cancellation or abandoned publication.
    expression: OwnedScalarExpr,
    memory: Option<MemoryReservation>,
}

impl Publication {
    pub(super) fn new(
        mut expression: OwnedScalarExpr,
        control: &ProductionControl<'_>,
    ) -> Result<Self> {
        expression.assert_owner(control);
        control.check()?;
        let memory = expression.memory.take().map(ScalarMemory::into_reservation);
        Ok(Self { expression, memory })
    }

    pub(super) fn finish(self, control: &ProductionControl<'_>) -> Result<Produced<ScalarExpr>> {
        let (value, memory) = self.expression.into_rewrite_parts();
        debug_assert!(memory.is_none());
        Ok(crate::plan::retirement::Pending::new(value).finish(self.memory, control)?)
    }
}
