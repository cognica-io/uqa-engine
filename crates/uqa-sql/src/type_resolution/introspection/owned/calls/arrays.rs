//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Array polymorphism and concatenation retain selected types while moving their operand owners.

use super::{ColumnType, FunctionDispatch, OwnedBinder, OwnedBindingCall, SQLError};
use crate::type_resolution::{
    common::unknown_input,
    concatenation,
    functions::{compatible_array_argument, compatible_array_result_type},
};

impl OwnedBinder<'_> {
    pub(super) fn coerce_compatible_array_call(
        &self,
        mut call: OwnedBindingCall,
    ) -> Result<OwnedBindingCall, SQLError> {
        if call.binding().is_some_and(|binding| !binding.builtin) {
            return Ok(call);
        }
        let name = self.local_name(call.name())?;
        let Some((family, width)) = super::super::super::arrays::layout(&name) else {
            return Ok(call);
        };
        if call.arguments().len() < width {
            return Ok(call);
        }
        for argument in &call.arguments()[..width] {
            if crate::scalar_call_argument(argument).is_ok_and(|argument| argument.name.is_some()) {
                return Ok(call);
            }
        }
        let known = self.common_arguments(&call.arguments()[..width])?;
        let Some(array) = self
            .context
            .semantic(compatible_array_result_type(
                family,
                &known,
                &self.context.control,
            ))?
            .flatten()
        else {
            return Ok(call);
        };
        let ColumnType::Array(element) = &*array else {
            return Ok(call);
        };
        for (position, ty) in known.iter().enumerate() {
            if ty.is_none() {
                let target = if compatible_array_argument(family, position) {
                    &*array
                } else {
                    element
                };
                call = call.map_argument(position, &self.context.control, |argument| {
                    self.common_cast(argument, target)
                })?;
            }
        }
        if &*name == "array_position" {
            if let Some(start) = call.arguments().get(2) {
                if self
                    .context
                    .semantic(self.context.common_context(start))?
                    .flatten()
                    .is_none()
                {
                    call = call.map_argument(2, &self.context.control, |argument| {
                        self.common_cast(argument, &ColumnType::Integer)
                    })?;
                }
            }
        }
        Ok(call)
    }

    pub(super) fn coerce_concat_call(
        &self,
        mut call: OwnedBindingCall,
    ) -> Result<OwnedBindingCall, SQLError> {
        let structural = matches!(
            call.binding().and_then(|binding| binding.dispatch),
            Some(
                FunctionDispatch::Concat { .. }
                    | FunctionDispatch::ArrayConcat
                    | FunctionDispatch::ArrayAppend
                    | FunctionDispatch::ArrayPrepend
            )
        );
        let legacy =
            concatenation::requires_binding(call.name(), call.binding(), self.context.resolver);
        if !structural && !legacy {
            return Ok(call);
        }
        let [left, right] = call.arguments() else {
            return Ok(call);
        };
        let left_type = self
            .context
            .semantic(self.context.common_context(left))?
            .flatten();
        let right_type = self
            .context
            .semantic(self.context.common_context(right))?
            .flatten();
        if (left_type.is_none() && !unknown_input(left))
            || (right_type.is_none() && !unknown_input(right))
        {
            return Ok(call);
        }
        let control = &self.context.control;
        let Some(selected) = self.context.semantic(concatenation::select(
            left_type.as_deref(),
            right_type.as_deref(),
            control,
        ))?
        else {
            return Ok(call);
        };
        for position in 0..2 {
            call = call.map_argument(position, control, |argument| {
                self.operand_cast(argument, selected.argument(position))
            })?;
        }
        call.rebind(selected.binding(control)?, control)
    }
}
