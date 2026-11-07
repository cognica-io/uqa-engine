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
        let mut next = self.take_owned_child();
        while let Some(mut current) = next {
            next = current.take_owned_child();
        }
    }
}
