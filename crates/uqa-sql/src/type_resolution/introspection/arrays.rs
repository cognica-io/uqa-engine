//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `anycompatiblearray` and `anycompatible` arguments of the built-in array functions: `unknown` literals and parameters take the element type selected by the known arguments, or its array type.

use super::{Binder, BindingCall, ColumnType, SQLError};
use crate::type_resolution::functions::{compatible_array_argument, compatible_array_result_type};

/// The `anycompatible` family layout of a built-in array function, named by the function whose argument roles it shares.
fn layout(name: &str) -> Option<(&'static str, usize)> {
    Some(match name {
        "array_append" => ("array_append", 2),
        "array_prepend" => ("array_prepend", 2),
        "array_cat" => ("array_cat", 2),
        "array_remove" | "array_position" | "array_positions" => ("array_remove", 2),
        "array_replace" => ("array_replace", 3),
        _ => return None,
    })
}

impl Binder<'_, '_> {
    pub(super) fn coerce_compatible_array_call(
        &mut self,
        call: &mut BindingCall,
    ) -> Result<(), SQLError> {
        if call
            .binding
            .as_ref()
            .is_some_and(|binding| !binding.builtin)
        {
            return Ok(());
        }
        let name = crate::type_resolution::common::local_routine_name(&call.name);
        let Some((family, width)) = layout(&name) else {
            return Ok(());
        };
        if call.arguments.len() < width {
            return Ok(());
        }
        let mut known = Vec::with_capacity(width);
        for argument in &call.arguments[..width] {
            if crate::scalar_call_argument(argument).is_ok_and(|argument| argument.name.is_some()) {
                return Ok(());
            }
            known.push(
                self.semantic(self.common_context(argument))?
                    .flatten()
                    .map(|ty| (*ty).clone()),
            );
        }
        let Some(array) = self
            .semantic(compatible_array_result_type(family, &known, &self.control))?
            .flatten()
        else {
            return Ok(());
        };
        let ColumnType::Array(element) = &*array else {
            return Ok(());
        };
        let element = (**element).clone();
        let array = (*array).clone();
        for (position, ty) in known.iter().enumerate() {
            if ty.is_none() {
                let target = if compatible_array_argument(family, position) {
                    &array
                } else {
                    &element
                };
                self.common_cast(&mut call.arguments[position], target)?;
            }
        }
        // `array_position(array, element, start)` takes an `integer` start position.
        if name == "array_position" {
            if let Some(start) = call.arguments.get_mut(2) {
                if self
                    .semantic(self.common_context(start))?
                    .flatten()
                    .is_none()
                {
                    self.common_cast(start, &ColumnType::Integer)?;
                }
            }
        }
        Ok(())
    }
}
