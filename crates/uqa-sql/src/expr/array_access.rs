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
    evaluate: impl FnMut(usize) -> Result<Produced<Value>, SQLError>,
) -> Result<Produced<Value>, SQLError> {
    eval_array_access_with_catalog_and_control(access, None, control, evaluate)
}

/// The value catalog interprets an admitted physical array after every reached bound has evaluated. It never repeats array or element input.
pub fn eval_array_access_with_catalog_and_control(
    access: ArrayAccess,
    catalog: Option<&dyn super::SQLValueCatalog>,
    control: &ProductionControl<'_>,
    mut evaluate: impl FnMut(usize) -> Result<Produced<Value>, SQLError>,
) -> Result<Produced<Value>, SQLError> {
    control.check()?;
    access.validate_dimensions()?;
    let array = evaluate(0)?;
    eval_array_access_from_input(access, &array, catalog, control, evaluate)
}

/// Already evaluated inputs stay borrowed; produced inputs retain their original guards until each conversion finishes.
pub(in crate::expr) fn eval_array_access_from_input<B: Deref<Target = Value>>(
    access: ArrayAccess,
    array: &Value,
    catalog: Option<&dyn super::SQLValueCatalog>,
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
    if (1..=access.bound_count())
        .any(|index| !access.is_omitted(index) && matches!(bounds[index - 1], Value::Null))
    {
        return Ok(control.finish(Value::Null, control.empty_reservation())?);
    }
    let decoded;
    let array = if let Value::Datum(datum) = array {
        decoded =
            super::datums::read_array_input_with_catalog_and_control(datum, catalog, control)?;
        &decoded
    } else {
        array
    };
    super::scalar_postgres::subscripts::eval_selected_array_access(
        array,
        &bounds[..access.bound_count()],
        access.slice_bounds(),
        control,
    )
}

#[cfg(test)]
mod tests;
