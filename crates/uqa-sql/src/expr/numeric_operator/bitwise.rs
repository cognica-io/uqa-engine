//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Integer bit operations use the selected SQL width, including `PostgreSQL`'s int2 shift promotion.

use super::{ColumnType, NumericOperator, Produced, ProductionControl, Result, SQLError, Value};

pub(super) fn evaluate(
    operator: NumericOperator,
    arguments: &[Value],
    result: &ColumnType,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    control.check()?;
    let Value::Int(left) = arguments[0] else {
        return Err(SQLError::Internal(
            "selected bitwise operand is not an integer".into(),
        ));
    };
    let right = if operator == NumericOperator::BitNot {
        0
    } else if let Value::Int(right) = arguments[1] {
        right
    } else {
        return Err(SQLError::Internal(
            "selected bitwise operand is not an integer".into(),
        ));
    };
    let value = match operator {
        NumericOperator::BitAnd => left & right,
        NumericOperator::BitOr => left | right,
        NumericOperator::BitXor => left ^ right,
        NumericOperator::BitNot => !left,
        NumericOperator::ShiftLeft | NumericOperator::ShiftRight => {
            // PostgreSQL promotes int2 to int32 before shifting, then truncates the result.
            // wrapping shifts implement the reference platform's masked count without C or Rust overflow.
            let count = right as u32;
            let left_shift = operator == NumericOperator::ShiftLeft;
            match result {
                ColumnType::SmallInteger | ColumnType::Integer => {
                    let left = i32::try_from(left).expect("converted int2/int4 operand");
                    i64::from(if left_shift {
                        left.wrapping_shl(count)
                    } else {
                        left.wrapping_shr(count)
                    })
                }
                ColumnType::BigInteger => {
                    if left_shift {
                        left.wrapping_shl(count)
                    } else {
                        left.wrapping_shr(count)
                    }
                }
                _ => {
                    return Err(SQLError::Internal(
                        "selected bitwise result is not an integer".into(),
                    ))
                }
            }
        }
        _ => unreachable!("only integer bitwise syntax selects this kernel"),
    };
    let value = match result {
        ColumnType::SmallInteger => i64::from(value as i16),
        ColumnType::Integer => i64::from(value as i32),
        ColumnType::BigInteger => value,
        _ => {
            return Err(SQLError::Internal(
                "selected bitwise result is not an integer".into(),
            ))
        }
    };
    Ok(control.finish(Value::Int(value), control.empty_reservation())?)
}
