//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Private call retirement removes one owned signature element or type wrapper per step.

use super::{FunctionBinding, FunctionValueBinding, RoutineInvocationBinding};

impl FunctionBinding {
    pub(crate) fn retire_step(&mut self) -> bool {
        if self.argument_types.pop().is_some() {
            return false;
        }
        if self
            .invocation
            .as_deref_mut()
            .is_some_and(|invocation| !invocation.retire_step())
        {
            return false;
        }
        match self.value_binding.as_deref_mut() {
            Some(FunctionValueBinding::CompositeField(field)) => {
                field.result_type.retire_step()
                    && field
                        .changed_type
                        .as_mut()
                        .is_none_or(super::super::ColumnType::retire_step)
            }
            _ => true,
        }
    }
}

impl RoutineInvocationBinding {
    fn retire_step(&mut self) -> bool {
        // Position and assignment-path buffers contain only Copy fields; destroying their allocation does not visit elements.
        self.argument_targets.pop().is_none()
            && self.argument_sources.pop().is_none()
            && self.parameter_types.pop().is_none()
    }
}
