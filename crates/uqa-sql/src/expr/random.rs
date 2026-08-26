//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! PostgreSQL-compatible stateful random-number functions.

use super::{
    out_of_range, to_decimal, to_i64, validate_named_argument_order, EvalContext, Result, SQLError,
    Value,
};
use crate::ast::FunctionDispatch;

pub(super) fn eval_random_function(
    name: &str,
    call_args: &[(Option<String>, Value)],
    ctx: &EvalContext<'_>,
) -> Option<Result<Value>> {
    if name == "random" && call_args.is_empty() {
        let engine = ctx.engine?;
        return match engine.random_value().map_err(SQLError::Internal) {
            Ok(Some(value)) => Some(Ok(Value::Float(value))),
            Ok(None) => None,
            Err(error) => Some(Err(error)),
        };
    }
    None
}

pub(super) fn eval_dispatched_random_function(
    dispatch: FunctionDispatch,
    call_args: &[(Option<String>, Value)],
    ctx: &EvalContext<'_>,
) -> Option<Result<Value>> {
    if !matches!(
        dispatch,
        FunctionDispatch::RandomInt4Range
            | FunctionDispatch::RandomInt8Range
            | FunctionDispatch::RandomNumericRange
    ) {
        return None;
    }
    Some((|| {
        let [lower, upper] = ordered_bounds(call_args)?;
        if matches!(lower, Value::Null) || matches!(upper, Value::Null) {
            return Ok(Value::Null);
        }
        match dispatch {
            FunctionDispatch::RandomInt4Range => {
                let lower = i32::try_from(to_i64(lower)?).map_err(|_| out_of_range("integer"))?;
                let upper = i32::try_from(to_i64(upper)?).map_err(|_| out_of_range("integer"))?;
                uniform_i64(i64::from(lower), i64::from(upper), ctx).map(Value::Int)
            }
            FunctionDispatch::RandomInt8Range => {
                uniform_i64(to_i64(lower)?, to_i64(upper)?, ctx).map(Value::Int)
            }
            FunctionDispatch::RandomNumericRange => {
                let lower = to_decimal(lower)?;
                let upper = to_decimal(upper)?;
                validate_numeric_bound(&lower, "lower")?;
                validate_numeric_bound(&upper, "upper")?;
                if lower > upper {
                    return Err(invalid_bounds());
                }
                lower
                    .uniform_sample_inclusive_with(&upper, || draw_u64(ctx))?
                    .map(Value::Decimal)
                    .ok_or_else(|| {
                        SQLError::Internal("numeric random range is not representable".into())
                    })
            }
            _ => unreachable!("random range dispatch was checked above"),
        }
    })())
}

fn ordered_bounds(call_args: &[(Option<String>, Value)]) -> Result<[&Value; 2]> {
    validate_named_argument_order(call_args.iter().map(|(name, _)| name.as_deref()))?;
    if call_args.len() != 2 {
        return Err(SQLError::BadArity {
            name: "random".into(),
            expected: "2".into(),
            actual: call_args.len(),
        });
    }
    let mut bounds = [None, None];
    let mut positional = 0;
    for (name, value) in call_args {
        let position = match name.as_deref() {
            Some("min") => 0,
            Some("max") => 1,
            Some(other) => {
                return Err(SQLError::Internal(format!(
                    "bound random range contains unknown argument `{other}`"
                )));
            }
            None => {
                let position = positional;
                positional += 1;
                position
            }
        };
        let Some(slot) = bounds.get_mut(position) else {
            return Err(SQLError::Internal(
                "bound random range contains too many positional arguments".into(),
            ));
        };
        if slot.replace(value).is_some() {
            return Err(SQLError::Internal(
                "bound random range assigns one argument more than once".into(),
            ));
        }
    }
    match bounds {
        [Some(lower), Some(upper)] => Ok([lower, upper]),
        _ => Err(SQLError::Internal(
            "bound random range is missing an argument".into(),
        )),
    }
}

fn uniform_i64(lower: i64, upper: i64, ctx: &EvalContext<'_>) -> Result<i64> {
    if lower > upper {
        return Err(invalid_bounds());
    }
    if lower == upper {
        return Ok(lower);
    }
    let range = (upper as u64).wrapping_sub(lower as u64);
    let shift = range.leading_zeros();
    let offset = loop {
        let candidate = draw_u64(ctx)? >> shift;
        if candidate <= range {
            break candidate;
        }
    };
    Ok((lower as u64).wrapping_add(offset) as i64)
}

fn draw_u64(ctx: &EvalContext<'_>) -> Result<u64> {
    if let Some(engine) = ctx.engine {
        if let Some(value) = engine.random_u64().map_err(SQLError::Internal)? {
            return Ok(value);
        }
    }
    let mut bytes = [0_u8; 8];
    getrandom::fill(&mut bytes)
        .map_err(|error| SQLError::Internal(format!("failed to obtain random bytes: {error}")))?;
    Ok(u64::from_le_bytes(bytes))
}

fn validate_numeric_bound(value: &uqa_core::DecimalValue, position: &str) -> Result<()> {
    if value.is_nan() {
        return Err(SQLError::Routine {
            sqlstate: "22023".into(),
            message: format!("{position} bound cannot be NaN"),
        });
    }
    if value.is_infinite() {
        return Err(SQLError::Routine {
            sqlstate: "22023".into(),
            message: format!("{position} bound cannot be infinity"),
        });
    }
    Ok(())
}

fn invalid_bounds() -> SQLError {
    SQLError::Routine {
        sqlstate: "22023".into(),
        message: "lower bound must be less than or equal to upper bound".into(),
    }
}
