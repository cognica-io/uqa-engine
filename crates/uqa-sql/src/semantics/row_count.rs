//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL LIMIT and OFFSET bigint conversion and diagnostics.

use crate::{SQLError, ScalarExpr};
use uqa_core::Value;

pub fn coerce_limit_offset(
    value: Value,
    expression: &ScalarExpr,
    label: &str,
) -> Result<Option<u64>, SQLError> {
    if matches!(value, Value::Null) {
        return Ok(None);
    }
    let allow_unknown_string = matches!(
        expression,
        ScalarExpr::Literal(Value::Str(_)) | ScalarExpr::Param(_)
    );
    let value = match &value {
        Value::Int(value) => Value::Int(*value),
        Value::Float(value) => Value::Int(
            i64::try_from(float_limit_offset(*value, label)?)
                .expect("PostgreSQL bigint row count fits i64"),
        ),
        Value::Decimal(decimal) if decimal.is_nan() => {
            return Err(SQLError::Routine {
                sqlstate: "0A000".into(),
                message: "cannot convert NaN to bigint".into(),
            });
        }
        Value::Decimal(decimal) if decimal.is_infinite() => {
            return Err(SQLError::Routine {
                sqlstate: "0A000".into(),
                message: "cannot convert infinity to bigint".into(),
            });
        }
        Value::Decimal(_) => crate::expr::cast_value(&value, "bigint")?,
        Value::Str(_) if allow_unknown_string => crate::expr::cast_value(&value, "bigint")?,
        other => {
            return Err(SQLError::TypeMismatch(format!(
                "argument of {label} must be type bigint, got {other:?}"
            )));
        }
    };
    let Value::Int(value) = value else {
        return Err(SQLError::Internal(
            "row-count bigint coercion did not return an integer".into(),
        ));
    };
    if value < 0 {
        return Err(negative_row_count(label));
    }
    Ok(Some(u64::try_from(value).map_err(|_| {
        SQLError::Internal("non-negative bigint did not fit u64".into())
    })?))
}

fn negative_row_count(label: &str) -> SQLError {
    if label == "OFFSET" {
        SQLError::Routine {
            sqlstate: "2201X".into(),
            message: "OFFSET must not be negative".into(),
        }
    } else {
        SQLError::Routine {
            sqlstate: "2201W".into(),
            message: "LIMIT must not be negative".into(),
        }
    }
}

pub fn float_limit_offset(value: f64, label: &str) -> Result<u64, SQLError> {
    let Value::Int(value) = crate::expr::cast_value(&Value::Float(value), "bigint")? else {
        return Err(SQLError::Internal(
            "float row-count coercion did not return an integer".into(),
        ));
    };
    if value < 0 {
        return Err(negative_row_count(label));
    }
    Ok(u64::try_from(value).expect("non-negative bigint fits u64"))
}

#[cfg(test)]
mod tests {
    use super::float_limit_offset;
    #[test]
    fn floating_limit_uses_postgresql_bigint_rounding_and_range_checks() {
        assert_eq!(float_limit_offset(42.0, "LIMIT").unwrap(), 42);
        assert_eq!(float_limit_offset(1.5, "LIMIT").unwrap(), 2);
        assert_eq!(float_limit_offset(2.5, "LIMIT").unwrap(), 2);
        assert_eq!(float_limit_offset(-0.5, "LIMIT").unwrap(), 0);
        for value in [f64::NAN, f64::INFINITY, -1.0, 9_223_372_036_854_775_808.0] {
            assert!(float_limit_offset(value, "LIMIT").is_err(), "{value}");
        }
    }
}
