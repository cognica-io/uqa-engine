//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native unary type ownership retires with constant stack and no temporary allocation.

use super::{ColumnType, ColumnTypeInputWork};
use uqa_core::memory::{Produced, ProductionControl};

impl Clone for ColumnType {
    fn clone(&self) -> Self {
        self.clone_with_control(&uqa_core::memory::ProductionControl::uncontrolled())
            .expect("ordinary type copy has no resource limit or cancellation")
            .into_uncontrolled()
            .expect("ordinary type copy has no payload lease")
    }
}

impl ColumnType {
    pub(crate) async fn retire_with_work<W: ColumnTypeInputWork>(
        value: Produced<Self>,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<(), W::Error> {
        let (value, _memory) = value.into_parts();
        let mut ty = value;
        while !ty.retire_step() {
            work.position(control).await?;
        }
        Ok(())
    }

    /// Retire one unary wrapper while retaining its child for the next work window.
    pub(crate) fn retire_step(&mut self) -> bool {
        let Some(child) = self.take_owned_child() else {
            return true;
        };
        *self = child;
        false
    }

    fn take_owned_child(&mut self) -> Option<Self> {
        match self {
            Self::Array(child) | Self::Domain { base: child, .. } => {
                Some(std::mem::replace(child.as_mut(), Self::Void))
            }
            _ => None,
        }
    }
}

impl Drop for ColumnType {
    fn drop(&mut self) {
        if let Some(mut child) = self.take_owned_child() {
            while !child.retire_step() {}
        }
    }
}
