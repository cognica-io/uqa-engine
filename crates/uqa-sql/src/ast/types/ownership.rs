//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native unary type ownership retires with constant stack and no temporary allocation.

use super::ColumnType;

impl Clone for ColumnType {
    fn clone(&self) -> Self {
        self.clone_with_control(&uqa_core::memory::ProductionControl::uncontrolled())
            .expect("ordinary type copy has no resource limit or cancellation")
            .into_uncontrolled()
            .expect("ordinary type copy has no payload lease")
    }
}

impl ColumnType {
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
