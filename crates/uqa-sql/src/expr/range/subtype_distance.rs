//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native range distance functions preserve `PostgreSQL`'s conversion order and temporal Datums.

use crate::{ast::RangeSubtype, expr::datums, ColumnType, SQLError};
use uqa_core::{
    memory::{Produced, ProductionControl},
    Value,
};

pub(in crate::expr) fn evaluate(
    name: &str,
    arguments: &[Value],
    control: &ProductionControl<'_>,
) -> Option<Result<Produced<Value>, SQLError>> {
    let subtype = match name {
        "int4range_subdiff" => RangeSubtype::Integer,
        "int8range_subdiff" => RangeSubtype::BigInteger,
        "numrange_subdiff" => RangeSubtype::Numeric,
        "daterange_subdiff" => RangeSubtype::Date,
        "tsrange_subdiff" => RangeSubtype::Timestamp,
        "tstzrange_subdiff" => RangeSubtype::TimestampTz,
        _ => return None,
    };
    Some(evaluate_selected(name, subtype, arguments, control))
}

fn evaluate_selected(
    name: &str,
    subtype: RangeSubtype,
    arguments: &[Value],
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    control.check()?;
    let [left, right] = arguments else {
        return Err(SQLError::BadArity {
            name: name.into(),
            expected: "2".into(),
            actual: arguments.len(),
        });
    };
    if matches!(left, Value::Null) || matches!(right, Value::Null) {
        return Ok(control.finish(Value::Null, control.empty_reservation())?);
    }
    let distance = if subtype == RangeSubtype::Numeric {
        numeric_distance(left, right, control)?
    } else {
        let ty = subtype.scalar_type();
        // Each original Datum is rounded before subtraction. Shifting the epoch or subtracting integers first changes observable rounding and infinite endpoint results.
        let difference = fixed_endpoint(left, &ty, control)? - fixed_endpoint(right, &ty, control)?;
        if matches!(subtype, RangeSubtype::Timestamp | RangeSubtype::TimestampTz) {
            difference / 1_000_000.0
        } else {
            difference
        }
    };
    Ok(control.finish(Value::Float(distance), control.empty_reservation())?)
}

fn fixed_endpoint(
    value: &Value,
    ty: &ColumnType,
    control: &ProductionControl<'_>,
) -> Result<f64, SQLError> {
    let bytes = datums::encoding::encode(value, ty, None, control)?;
    match &*bytes {
        [a, b, c, d] => Ok(f64::from(i32::from_le_bytes([*a, *b, *c, *d]))),
        bytes if bytes.len() == 8 => {
            Ok(i64::from_le_bytes(bytes.try_into().expect("eight-byte endpoint")) as f64)
        }
        _ => Err(SQLError::Internal(
            "invalid fixed range endpoint width".into(),
        )),
    }
}

fn numeric_distance(
    left: &Value,
    right: &Value,
    control: &ProductionControl<'_>,
) -> Result<f64, SQLError> {
    let decoded_left = decode_numeric(left, control)?;
    let decoded_right = decode_numeric(right, control)?;
    let (Value::Decimal(left), Value::Decimal(right)) = (
        decoded_left.as_deref().unwrap_or(left),
        decoded_right.as_deref().unwrap_or(right),
    ) else {
        return Err(SQLError::Internal("invalid numeric range endpoint".into()));
    };
    let difference = left
        .checked_sub_with_control(right, control)?
        .ok_or_else(|| SQLError::Routine {
            sqlstate: "22003".into(),
            message: "value overflows numeric format".into(),
        })?;
    let (difference, memory) = difference.into_parts();
    let difference = control.finish(Value::Decimal(difference), memory)?;
    crate::expr::floating::to_float_with_control(
        &difference,
        crate::expr::FloatWidth::DoublePrecision,
        control,
    )
}

fn decode_numeric(
    value: &Value,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<Value>>, SQLError> {
    match value {
        Value::Datum(datum) => datums::read_with_control(datum, control).map(Some),
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests;
