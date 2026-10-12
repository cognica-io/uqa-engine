//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Evaluate array access once, retaining `PostgreSQL`'s ordered bound effects.

use std::ops::Deref;
use uqa_core::{
    memory::{Produced, ProductionControl},
    Value,
};

use crate::{
    ast::{ArrayAccess, MAX_ARRAY_SUBSCRIPT_DIMENSIONS},
    SQLError,
};

/// The caller owns expression scheduling and input guards; SQL owns NULL reachability, bound conversion and selected array output.
pub fn eval_array_access_with_control(
    access: ArrayAccess,
    control: &ProductionControl<'_>,
    mut evaluate: impl FnMut(usize) -> Result<Produced<Value>, SQLError>,
) -> Result<Produced<Value>, SQLError> {
    control.check()?;
    access.validate_dimensions()?;
    let array = evaluate(0)?;
    eval_array_access_from_input(access, &array, control, evaluate)
}

/// Already evaluated inputs stay borrowed; produced inputs retain their original guards until each conversion finishes.
pub(in crate::expr) fn eval_array_access_from_input<B: Deref<Target = Value>>(
    access: ArrayAccess,
    array: &Value,
    control: &ProductionControl<'_>,
    mut evaluate: impl FnMut(usize) -> Result<B, SQLError>,
) -> Result<Produced<Value>, SQLError> {
    control.check()?;
    access.validate_dimensions()?;
    if matches!(array, Value::Null) {
        return Ok(control.finish(Value::Null, control.empty_reservation())?);
    }
    let mut bounds: [Value; MAX_ARRAY_SUBSCRIPT_DIMENSIONS * 2] =
        std::array::from_fn(|_| Value::Null);
    for index in access.evaluation_order() {
        control.check()?;
        let value = evaluate(index)?;
        let value = super::cast_value_from_with_control(&value, "integer", None, control)?;
        bounds[index - 1] = match *value {
            Value::Null => Value::Null,
            Value::Int(value) => Value::Int(value),
            _ => {
                return Err(SQLError::Internal(
                    "array bound coercion did not produce an integer".into(),
                ))
            }
        };
    }
    super::scalar_postgres::subscripts::eval_selected_array_access(
        array,
        &bounds[..access.bound_count()],
        access.slice_bounds(),
        control,
    )
}

#[cfg(test)]
mod tests;
