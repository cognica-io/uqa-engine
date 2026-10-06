//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain `||` input conversions and its selected array operation before runtime values erase their declared types.

use super::{Binder, BindingCall, ColumnType, FunctionBinding, SQLError};
use crate::type_resolution::{
    common::local_routine_name,
    functions::{array_concat_argument, array_concat_types, concat_argument_type},
    is_unknown_literal,
};

impl Binder<'_, '_> {
    pub(super) fn coerce_concat_call(&mut self, call: &mut BindingCall) -> Result<(), SQLError> {
        if call
            .binding
            .as_ref()
            .is_some_and(|binding| !binding.builtin)
            || local_routine_name(&call.name) != "concat_op"
        {
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
        if let Some((dispatch, array)) = self
            .semantic(array_concat_types(
                left_type.as_deref(),
                right_type.as_deref(),
                &self.control,
            ))?
            .flatten()
        {
            let ColumnType::Array(element) = &*array else {
                unreachable!("array concatenation result");
            };
            for (position, operand) in [left, right].into_iter().enumerate() {
                let target = if array_concat_argument(dispatch, position) {
                    &*array
                } else {
                    element
                };
                self.operand_cast(operand, target)?;
            }
            let binding = FunctionBinding::dispatched_with_control(dispatch, &self.control)?;
            call.binding = Some(self.retain(binding));
            return Ok(());
        }
        let (unknown, typed) = match (is_unknown_literal(left), is_unknown_literal(right)) {
            (true, false) => (left, right),
            (false, true) => (right, left),
            // Two `unknown` operands select `text || text`, which the deparser and evaluation read as text; two typed operands select by their own types.
            _ => return Ok(()),
        };
        let Some(other) = self.semantic(self.common_context(typed))?.flatten() else {
            return Ok(());
        };
        self.operand_cast(unknown, &concat_argument_type(Some(&other)))
    }
}
