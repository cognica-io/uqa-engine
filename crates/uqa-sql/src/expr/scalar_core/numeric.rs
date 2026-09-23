//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::plain;
use crate::{
    error::{Result, SQLError},
    expr::{
        conversion::{gcd_i64, to_decimal_with_control, to_f64_with_control, to_i64_with_control},
        division_by_zero, out_of_range,
    },
};
use uqa_core::{
    memory::{Produced, ProductionControl},
    DecimalValue, Value,
};

fn decimal(
    value: Produced<DecimalValue>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    let (value, memory) = value.into_parts();
    Ok(control.finish(Value::Decimal(value), memory)?)
}

pub(super) fn evaluate(
    name: &str,
    args: &[Value],
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    match name {
        "abs" | "ceil" | "ceiling" | "floor" => unary(name, &args[0], control),
        "round" => round(args, control),
        "power" | "pow" => power(args, control),
        "sqrt" => {
            if args.len() != 1 {
                return Err(SQLError::TypeMismatch("sqrt takes 1 arg".into()));
            }
            if matches!(args[0], Value::Null) {
                return plain(Value::Null, control);
            }
            plain(
                Value::Float(crate::expr::floating::square_root(to_f64_with_control(
                    &args[0], control,
                )?)?),
                control,
            )
        }
        "mod" => remainder(args, control),
        "div" | "gcd" | "lcm" => integer(name, args, control),
        _ => unreachable!("numeric family membership was checked"),
    }
}

fn unary(name: &str, value: &Value, control: &ProductionControl<'_>) -> Result<Produced<Value>> {
    let scalar = match (name, value) {
        (_, Value::Null) => Value::Null,
        ("abs", Value::Int(value)) => {
            Value::Int(value.checked_abs().ok_or_else(|| out_of_range("bigint"))?)
        }
        (_, Value::Int(value)) => Value::Int(*value),
        ("abs", Value::Float(value)) => Value::Float(value.abs()),
        ("ceil" | "ceiling", Value::Float(value)) => Value::Float(value.ceil()),
        ("floor", Value::Float(value)) => Value::Float(value.floor()),
        ("abs", Value::Decimal(value)) => {
            return decimal(value.abs_with_control(control)?, control);
        }
        ("ceil" | "ceiling", Value::Decimal(value)) => {
            return decimal(value.ceil_with_control(control)?, control);
        }
        ("floor", Value::Decimal(value)) => {
            return decimal(value.floor_with_control(control)?, control);
        }
        ("abs", other) => {
            return Err(SQLError::TypeMismatch(format!(
                "abs() expected number, got {other:?}"
            )));
        }
        ("ceil" | "ceiling", other) => {
            return Err(SQLError::TypeMismatch(format!("ceil({other:?})")));
        }
        ("floor", other) => return Err(SQLError::TypeMismatch(format!("floor({other:?})"))),
        _ => unreachable!("unary numeric family"),
    };
    plain(scalar, control)
}

fn round(args: &[Value], control: &ProductionControl<'_>) -> Result<Produced<Value>> {
    match args.len() {
        1 => match &args[0] {
            Value::Int(value) => plain(Value::Int(*value), control),
            Value::Float(value) => plain(Value::Float(value.round_ties_even()), control),
            Value::Decimal(value) => match value.round_to_scale_with_control(0, control)? {
                Some(value) => decimal(value, control),
                None => Ok(control.copy_value(&args[0])?),
            },
            Value::Null => plain(Value::Null, control),
            other => Err(SQLError::TypeMismatch(format!("round({other:?})"))),
        },
        2 => {
            if args.iter().any(|value| matches!(value, Value::Null)) {
                return plain(Value::Null, control);
            }
            if let Value::Decimal(value) = &args[0] {
                let places = to_i64_with_control(&args[1], control)?;
                let places = i32::try_from(places).map_err(|_| {
                    SQLError::TypeMismatch(format!("round scale out of range: {places}"))
                })?;
                return decimal(
                    value
                        .round_to_scale_with_control(places, control)?
                        .ok_or_else(|| SQLError::TypeMismatch("decimal round overflow".into()))?,
                    control,
                );
            }
            let value = to_f64_with_control(&args[0], control)?;
            let places = i32::try_from(to_i64_with_control(&args[1], control)?)
                .map_err(|_| out_of_range("integer"))?;
            let scale = 10_f64.powi(places);
            plain(Value::Float((value * scale).round() / scale), control)
        }
        _ => Err(SQLError::TypeMismatch("round takes 1-2 args".into())),
    }
}

fn power(args: &[Value], control: &ProductionControl<'_>) -> Result<Produced<Value>> {
    if args.len() != 2 {
        return Err(SQLError::TypeMismatch("power takes 2 args".into()));
    }
    if args.iter().any(|value| matches!(value, Value::Null)) {
        return plain(Value::Null, control);
    }
    if args.iter().any(|value| matches!(value, Value::Decimal(_)))
        && !args.iter().any(|value| matches!(value, Value::Float(_)))
    {
        let base = to_decimal_with_control(&args[0], control)?;
        let exponent = to_decimal_with_control(&args[1], control)?;
        if base.is_zero() && exponent.is_negative() {
            return Err(invalid_power(
                "zero raised to a negative power is undefined",
            ));
        }
        if base.is_negative()
            && !exponent.is_nan()
            && !exponent.is_integral_with_control(control)?
        {
            return Err(invalid_power(
                "a negative number raised to a non-integer power yields a complex result",
            ));
        }
        let result = base
            .checked_pow_postgres_with_control(&exponent, control)?
            .ok_or_else(|| SQLError::Routine {
                sqlstate: "22003".into(),
                message: "value overflows numeric format".into(),
            })?;
        return decimal(result, control);
    }
    plain(
        Value::Float(crate::expr::floating::power(
            to_f64_with_control(&args[0], control)?,
            to_f64_with_control(&args[1], control)?,
        )?),
        control,
    )
}

fn invalid_power(message: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "2201F".into(),
        message: message.into(),
    }
}

fn remainder(args: &[Value], control: &ProductionControl<'_>) -> Result<Produced<Value>> {
    if args.len() != 2 {
        return Err(SQLError::TypeMismatch("mod takes 2 args".into()));
    }
    if args.iter().any(|value| matches!(value, Value::Null)) {
        return plain(Value::Null, control);
    }
    let scalar = match (&args[0], &args[1]) {
        (Value::Int(_), Value::Int(0)) => return Err(division_by_zero()),
        (Value::Int(_), Value::Int(-1)) => Value::Int(0),
        (Value::Int(left), Value::Int(right)) => Value::Int(
            left.checked_rem(*right)
                .ok_or_else(|| out_of_range("bigint"))?,
        ),
        (left, right)
            if matches!(left, Value::Decimal(_)) || matches!(right, Value::Decimal(_)) =>
        {
            let divisor = to_decimal_with_control(right, control)?;
            if divisor.is_zero() {
                return Err(division_by_zero());
            }
            let dividend = to_decimal_with_control(left, control)?;
            return decimal(
                dividend
                    .checked_rem_with_control(&divisor, control)?
                    .ok_or_else(|| out_of_range("numeric"))?,
                control,
            );
        }
        (left, right) => {
            let left = to_f64_with_control(left, control)?;
            let right = to_f64_with_control(right, control)?;
            if right == 0.0 {
                return Err(division_by_zero());
            }
            Value::Float(left % right)
        }
    };
    plain(scalar, control)
}

fn integer(name: &str, args: &[Value], control: &ProductionControl<'_>) -> Result<Produced<Value>> {
    if args.len() != 2 {
        return Err(SQLError::TypeMismatch(format!("{name} takes 2 args")));
    }
    let value = match name {
        "div" => {
            let divisor = to_i64_with_control(&args[1], control)?;
            if divisor == 0 {
                return Err(division_by_zero());
            }
            to_i64_with_control(&args[0], control)?
                .checked_div(divisor)
                .ok_or_else(|| out_of_range("bigint"))?
        }
        "gcd" => gcd_i64(
            to_i64_with_control(&args[0], control)?,
            to_i64_with_control(&args[1], control)?,
        )?,
        "lcm" => {
            let left = to_i64_with_control(&args[0], control)?;
            let right = to_i64_with_control(&args[1], control)?;
            if left == 0 || right == 0 {
                0
            } else {
                let gcd = i128::from(gcd_i64(left, right)?);
                (i128::from(left) / gcd)
                    .checked_mul(i128::from(right))
                    .and_then(i128::checked_abs)
                    .and_then(|value| i64::try_from(value).ok())
                    .ok_or_else(|| out_of_range("bigint"))?
            }
        }
        _ => unreachable!("integer numeric family"),
    };
    plain(Value::Int(value), control)
}
