//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    not_an_array, out_of_range, to_i64_with_control, ProductionControl, Result, SQLError, Value,
};

pub(super) fn evaluate(
    name: &str,
    args: &[Value],
    control: &ProductionControl<'_>,
) -> Result<Value> {
    match name {
        "array_length" | "array_upper" | "array_lower" => {
            if args.len() != 2 {
                return Err(SQLError::TypeMismatch(format!("{name} takes 2 args")));
            }
            let array = match &args[0] {
                Value::Array(array) => array,
                Value::LegacyVector(vector) => vector.as_array(),
                Value::Null => return Ok(Value::Null),
                other => return Err(not_an_array(name, other)),
            };
            if matches!(args[1], Value::Null) {
                return Ok(Value::Null);
            }
            let Some(dimension) = dimension_index(&args[1], control)? else {
                return Ok(Value::Null);
            };
            let Some(length) = array.dimensions().get(dimension) else {
                return Ok(Value::Null);
            };
            match name {
                "array_length" => i64::try_from(*length)
                    .map(Value::Int)
                    .map_err(|_| out_of_range("array length")),
                "array_lower" => array
                    .lower_bound(dimension)
                    .map(|bound| Value::Int(i64::from(bound)))
                    .ok_or_else(|| SQLError::TypeMismatch("invalid array dimensions".into())),
                "array_upper" => Ok(array
                    .upper_bound(dimension)
                    .map(Value::Int)
                    .unwrap_or(Value::Null)),
                _ => unreachable!(),
            }
        }
        "array_ndims" => {
            if args.len() != 1 {
                return Err(SQLError::TypeMismatch("array_ndims takes 1 arg".into()));
            }
            match args[0].array_view() {
                Some(array) if array.dimensions().is_empty() => Ok(Value::Null),
                Some(array) => i64::try_from(array.dimensions().len())
                    .map(Value::Int)
                    .map_err(|_| out_of_range("array dimensions")),
                None if matches!(args[0], Value::Null) => Ok(Value::Null),
                None => Err(not_an_array("array_ndims", &args[0])),
            }
        }
        "cardinality" => {
            if args.len() != 1 {
                return Err(SQLError::TypeMismatch("cardinality takes 1 arg".into()));
            }
            match args[0].array_view() {
                Some(array) => {
                    let cardinality = array.dimensions().iter().try_fold(
                        i64::from(!array.dimensions().is_empty()),
                        |total, length| {
                            control.check()?;
                            let length = i64::try_from(*length)
                                .map_err(|_| out_of_range("array cardinality"))?;
                            total
                                .checked_mul(length)
                                .ok_or_else(|| out_of_range("array cardinality"))
                        },
                    )?;
                    Ok(Value::Int(cardinality))
                }
                None if matches!(args[0], Value::Null) => Ok(Value::Null),
                None => Err(not_an_array("cardinality", &args[0])),
            }
        }
        _ => unreachable!("scalar array property"),
    }
}

fn dimension_index(value: &Value, control: &ProductionControl<'_>) -> Result<Option<usize>> {
    let dimension = to_i64_with_control(value, control)?;
    if dimension <= 0 {
        return Ok(None);
    }
    Ok(usize::try_from(dimension - 1).ok())
}
