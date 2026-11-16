//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Frozen scalars keep iterative retirement until their final fallible publication completes.

use super::{
    CatalogRetentionError, MemoryReservation, OwnedScalarExpr, Produced, ProductionControl, Result,
    ScalarExpr, ScalarMemory,
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
        let value = control.finish(self.expression, self.memory)?;
        match value.into_budgeted() {
            Ok(value) => {
                // Publication already checked cancellation while the iterative owner was live. This unwrap moves the identical payload and lease without another fallible check or allocation.
                Ok(value
                    .try_map(|value| Ok::<_, CatalogRetentionError>(value.into_rewrite_parts().0))?
                    .into())
            }
            Err(value) => {
                let value = value
                    .into_uncontrolled()
                    .expect("ordinary scalar publication");
                Ok(control.finish(value.into_rewrite_parts().0, None)?)
            }
        }
    }
}
