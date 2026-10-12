//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Selected concatenation keeps native carrier operations and catalog-aware scalar output separate.

use super::{EvalContext, Result, SQLError};
use crate::ast::FunctionBinding;
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionString},
    Value,
};

pub(super) fn evaluate(
    binding: &FunctionBinding,
    oid: Option<u32>,
    arguments: &[Value],
    control: &ProductionControl<'_>,
    context: &EvalContext<'_>,
) -> Result<Produced<Value>> {
    control.check()?;
    let [left, right] = arguments else {
        return Err(SQLError::Internal("invalid concatenation operands".into()));
    };
    let mixed = match oid {
        Some(2779) => Some(1),
        Some(2780) => Some(0),
        Some(654 | 2018 | 3284) | None => None,
        Some(_) => {
            return Err(SQLError::Internal(
                "invalid scalar concatenation identity".into(),
            ))
        }
    };
    if let Some(position) = mixed {
        let [left_type, right_type] = binding.argument_types.as_slice() else {
            return Err(SQLError::Internal(
                "concatenation lost its selected input types".into(),
            ));
        };
        if matches!(left, Value::Null) || matches!(right, Value::Null) {
            return Ok(control.finish(Value::Null, control.empty_reservation())?);
        }
        let text = super::cast_value_with_type_resolution_with_control(
            &arguments[position],
            Some([left_type, right_type][position]),
            "text",
            context.engine,
            control,
        )?;
        let Value::Str(converted) = &*text else {
            return Err(SQLError::Internal(
                "concatenation input did not produce text".into(),
            ));
        };
        let Value::Str(other) = &arguments[1 - position] else {
            return Err(SQLError::Internal(
                "concatenation text operand has an invalid carrier".into(),
            ));
        };
        let mut output = ProductionString::new(*control);
        for part in if position == 0 {
            [converted, other]
        } else {
            [other, converted]
        } {
            output.push_str(part)?;
        }
        let (text, memory) = output.finish()?.into_parts();
        return Ok(control.finish(Value::Str(text), memory)?);
    }
    super::scalar_core::eval_core_functions_with_control("concat_op", arguments, control)
        .expect("existing concatenation kernel")
}

#[cfg(test)]
mod tests;
