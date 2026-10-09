//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{not_an_array, out_of_range, to_i64_with_control, ProductionControl, Result, Value};

pub(super) fn evaluate(
    name: &str,
    args: &[Value],
    control: &ProductionControl<'_>,
) -> Result<Value> {
    let arity = if matches!(name, "array_length" | "array_upper" | "array_lower") {
        2
    } else {
        1
    };
    super::require_arity(name, args, arity)?;
    if args.iter().any(|value| matches!(value, Value::Null)) {
        return Ok(Value::Null);
    }
    let physical;
    let (dimensions, bounds) = if let Value::Datum(datum) = &args[0] {
        physical = super::super::datums::array_shape(datum, control)?;
        (physical.dimensions(), physical.lower_bounds())
    } else {
        let array = args[0]
            .array_view()
            .ok_or_else(|| not_an_array(name, &args[0]))?;
        (array.dimensions(), array.lower_bounds())
    };
    match name {
        "array_length" | "array_upper" | "array_lower" => {
            let Some(dimension) = dimension_index(&args[1], control)? else {
                return Ok(Value::Null);
            };
            let Some(length) = dimensions.get(dimension) else {
                return Ok(Value::Null);
            };
            let length = i64::try_from(*length).map_err(|_| out_of_range("array length"))?;
            let lower = i64::from(bounds[dimension]);
            Ok(Value::Int(match name {
                "array_length" => length,
                "array_lower" => lower,
                _ => lower
                    .checked_add(length)
                    .and_then(|upper| upper.checked_sub(1))
                    .ok_or_else(|| out_of_range("array upper bound"))?,
            }))
        }
        "array_ndims" => Ok(if dimensions.is_empty() {
            Value::Null
        } else {
            Value::Int(dimensions.len() as i64)
        }),
        "cardinality" => dimensions
            .iter()
            .try_fold(i64::from(!dimensions.is_empty()), |total, length| {
                control.check()?;
                let length =
                    i64::try_from(*length).map_err(|_| out_of_range("array cardinality"))?;
                total
                    .checked_mul(length)
                    .ok_or_else(|| out_of_range("array cardinality"))
            })
            .map(Value::Int),
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
