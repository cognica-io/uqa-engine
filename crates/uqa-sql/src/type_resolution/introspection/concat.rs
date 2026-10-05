//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The `unknown` operand of `||` takes the type of the typed operand, as `oper_select_candidate` selects the operator: `bytea || bytea`, `jsonb || jsonb` and the array operators match the typed operand exactly where `anynonarray || text` and `text || anynonarray` do not, and every other operand type selects the `text` operand. The literal is read by the selected type's input function and stored as its constant.

use super::{Binder, BindingCall, SQLError};
use crate::type_resolution::{
    common::local_routine_name, functions::concat_argument_type, is_unknown_literal,
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
