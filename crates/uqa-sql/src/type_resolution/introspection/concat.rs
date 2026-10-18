//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain `||` input conversions and its selected array operation before runtime values erase their declared types.

use super::{Binder, BindingCall, FunctionBinding, SQLError};
use crate::ast::FunctionDispatch;
use crate::type_resolution::concatenation;

impl Binder<'_, '_> {
    pub(super) fn coerce_concat_call(&mut self, call: &mut BindingCall) -> Result<(), SQLError> {
        let structural = matches!(
            call.binding.as_ref().and_then(|binding| binding.dispatch),
            Some(
                FunctionDispatch::Concat { .. }
                    | FunctionDispatch::ArrayConcat
                    | FunctionDispatch::ArrayAppend
                    | FunctionDispatch::ArrayPrepend
            )
        );
        let legacy =
            concatenation::requires_binding(&call.name, call.binding.as_ref(), self.resolver);
        if !structural && !legacy {
            return Ok(());
        }
        let [left, right] = call.arguments.as_mut_slice() else {
            return Ok(());
        };
        let left_type = self.semantic(self.common_context(left))?.flatten();
        let right_type = self.semantic(self.common_context(right))?.flatten();
        // A schema-less column is a dynamic carrier, not an unknown SQL literal or parameter.
        if (left_type.is_none() && !super::comparison::unknown_input(left))
            || (right_type.is_none() && !super::comparison::unknown_input(right))
        {
            return Ok(());
        }
        let Some(selected) = self.semantic(concatenation::select(
            left_type.as_deref(),
            right_type.as_deref(),
            &self.control,
        ))?
        else {
            return Ok(());
        };
        for (position, operand) in [left, right].into_iter().enumerate() {
            self.operand_cast(operand, selected.argument(position))?;
        }
        let binding = FunctionBinding::dispatched_with_control(selected.dispatch(), &self.control)?;
        let mut binding = self.retain(binding);
        if matches!(
            binding.dispatch,
            Some(FunctionDispatch::Concat {
                oid: Some(2779 | 2780)
            })
        ) {
            let mut names = uqa_core::memory::ProductionVec::new(self.control);
            names.reserve(2)?;
            for position in 0..2 {
                names.push_produced(
                    selected
                        .argument(position)
                        .catalog_name_with_control(&self.control)?,
                )?;
            }
            binding.argument_types = self.retain(names.finish()?);
        }
        crate::ast::FunctionBinding::rebind(&mut call.binding, binding);
        Ok(())
    }
}
