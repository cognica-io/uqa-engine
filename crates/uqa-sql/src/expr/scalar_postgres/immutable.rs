//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable scalar leaves share their ordinary producers and keep output and scratch under the caller's control.

use crate::{
    ast::FunctionDispatch,
    expr::{
        conversion::{to_i64_with_control, value_to_string_with_control},
        out_of_range,
        scalar_helpers::{quote_ident_with_control, quote_literal_with_control},
        ArrayValue, DecimalValue, Result, SQLError, Value,
    },
};
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};

pub(in crate::expr) fn eval_postgres_immutable_with_control(
    name: &str,
    args: &[Value],
    control: &ProductionControl<'_>,
) -> Option<Result<Produced<Value>>> {
    if !matches!(
        name,
        "factorial"
            | "bit_length"
            | "to_bin"
            | "to_hex"
            | "to_oct"
            | "string_to_array"
            | "quote_ident"
            | "quote_literal"
            | "quote_nullable"
            | "num_nulls"
            | "num_nonnulls"
    ) {
        return None;
    }
    Some((|| {
        control.check()?;
        match name {
            "factorial" => factorial(args, control),
            "bit_length" => bit_length(args, control),
            "to_bin" | "to_hex" | "to_oct" => Err(SQLError::Internal(format!(
                "{name} reached runtime before its integer overload was bound"
            ))),
            "string_to_array" => string_to_array(args, control),
            "quote_ident" | "quote_literal" | "quote_nullable" => quote(name, args, control),
            "num_nulls" | "num_nonnulls" => {
                let count_null = name == "num_nulls";
                let mut count = 0;
                for value in args {
                    control.check()?;
                    count += i64::from(matches!(value, Value::Null) == count_null);
                }
                inline(Value::Int(count), control)
            }
            _ => unreachable!("immutable family checked before dispatch"),
        }
    })())
}

fn inline(value: Value, control: &ProductionControl<'_>) -> Result<Produced<Value>> {
    Ok(control.finish(value, control.empty_reservation())?)
}

fn string(value: Produced<String>, control: &ProductionControl<'_>) -> Result<Produced<Value>> {
    let (value, memory) = value.into_parts();
    Ok(control.finish(Value::Str(value), memory)?)
}

fn factorial(args: &[Value], control: &ProductionControl<'_>) -> Result<Produced<Value>> {
    let [value] = args else {
        return Err(SQLError::TypeMismatch("factorial takes 1 arg".into()));
    };
    if matches!(value, Value::Null) {
        return inline(Value::Null, control);
    }
    let n = to_i64_with_control(value, control)?;
    if n < 0 {
        return Err(SQLError::Routine {
            sqlstate: "2201F".into(),
            message: "factorial of a negative number is undefined".into(),
        });
    }
    let mut accumulator = 1_i128;
    for factor in 2..=i128::from(n) {
        control.check()?;
        accumulator = accumulator
            .checked_mul(factor)
            .ok_or_else(|| out_of_range("numeric"))?;
    }
    if let Ok(small) = i64::try_from(accumulator) {
        return inline(Value::Int(small), control);
    }
    let text = control.format(format_args!("{accumulator}"))?;
    let value =
        DecimalValue::parse_with_control(&text, control)?.ok_or_else(|| out_of_range("numeric"))?;
    let (value, memory) = value.into_parts();
    Ok(control.finish(Value::Decimal(value), memory)?)
}

fn bit_length(args: &[Value], control: &ProductionControl<'_>) -> Result<Produced<Value>> {
    let [value] = args else {
        return Err(SQLError::TypeMismatch("bit_length takes 1 arg".into()));
    };
    let octets = match value {
        Value::Null => return inline(Value::Null, control),
        Value::Str(text) => text.len(),
        Value::FixedChar(text) => text.trim_end_matches(' ').len(),
        Value::Bytes(bytes) => bytes.len(),
        _ => {
            return Err(SQLError::TypeMismatch(
                "bit_length requires text or bytea".into(),
            ));
        }
    };
    inline(Value::Int(octets as i64 * 8), control)
}

pub(in crate::expr) fn eval_postgres_integer_base_with_control(
    dispatch: FunctionDispatch,
    args: &[Value],
    control: &ProductionControl<'_>,
) -> Option<Result<Produced<Value>>> {
    if !matches!(
        dispatch,
        FunctionDispatch::ToBinInt4
            | FunctionDispatch::ToBinInt8
            | FunctionDispatch::ToHexInt4
            | FunctionDispatch::ToHexInt8
            | FunctionDispatch::ToOctInt4
            | FunctionDispatch::ToOctInt8
    ) {
        return None;
    }
    Some((|| {
        control.check()?;
        let [argument] = args else {
            return Err(SQLError::TypeMismatch(format!(
                "{} takes 1 arg",
                dispatch.label()
            )));
        };
        if matches!(argument, Value::Null) {
            return inline(Value::Null, control);
        }
        let value = to_i64_with_control(argument, control)?;
        let value = match dispatch {
            FunctionDispatch::ToBinInt4
            | FunctionDispatch::ToHexInt4
            | FunctionDispatch::ToOctInt4 => {
                u64::from(i32::try_from(value).map_err(|_| out_of_range("integer"))? as u32)
            }
            _ => value as u64,
        };
        let text = match dispatch {
            FunctionDispatch::ToBinInt4 | FunctionDispatch::ToBinInt8 => {
                control.format(format_args!("{value:b}"))?
            }
            FunctionDispatch::ToHexInt4 | FunctionDispatch::ToHexInt8 => {
                control.format(format_args!("{value:x}"))?
            }
            FunctionDispatch::ToOctInt4 | FunctionDispatch::ToOctInt8 => {
                control.format(format_args!("{value:o}"))?
            }
            _ => unreachable!("integer base family checked before formatting"),
        };
        string(text, control)
    })())
}

pub(super) fn string_to_array(
    args: &[Value],
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    control.check()?;
    if !(2..=3).contains(&args.len()) {
        return Err(SQLError::TypeMismatch(
            "string_to_array takes 2-3 args".into(),
        ));
    }
    if matches!(args[0], Value::Null) {
        return inline(Value::Null, control);
    }
    let text = value_to_string_with_control(&args[0], control)?;
    let null_marker = args.get(2).filter(|value| !matches!(value, Value::Null));
    let mut marker = None;
    let mut values = ProductionVec::new(*control);
    let mut push = |part: &str| -> Result<()> {
        control.check()?;
        if marker.is_none() {
            if let Some(null_marker) = null_marker {
                marker = Some(value_to_string_with_control(null_marker, control)?);
            }
        }
        let value = if marker
            .as_deref()
            .is_some_and(|marker: &String| marker == part)
        {
            inline(Value::Null, control)?
        } else {
            string(control.copy_text(part)?, control)?
        };
        values.push_produced(value)?;
        Ok(())
    };
    match &args[1] {
        Value::Null => {
            for character in text.chars() {
                let mut bytes = [0; 4];
                push(character.encode_utf8(&mut bytes))?;
            }
        }
        separator => {
            let separator = value_to_string_with_control(separator, control)?;
            if !text.is_empty() {
                if separator.is_empty() {
                    push(&text)?;
                } else {
                    for part in text.split(separator.as_str()) {
                        push(part)?;
                    }
                }
            }
        }
    }
    let array = ArrayValue::try_new_with_control(values.finish()?, control)?
        .ok_or_else(|| SQLError::TypeMismatch("invalid string_to_array result".into()))?;
    let (array, memory) = array.into_parts();
    Ok(control.finish(Value::Array(array), memory)?)
}

fn quote(name: &str, args: &[Value], control: &ProductionControl<'_>) -> Result<Produced<Value>> {
    let value = match args.first() {
        Some(Value::Null) | None if name == "quote_nullable" => {
            return string(control.copy_text("NULL")?, control);
        }
        Some(Value::Null) => return inline(Value::Null, control),
        Some(value) => value,
        None => return Err(SQLError::TypeMismatch("missing arg #0".into())),
    };
    let text = value_to_string_with_control(value, control)?;
    let quoted = if name == "quote_ident" {
        quote_ident_with_control(&text, control)?
    } else {
        quote_literal_with_control(&text, control)?
    };
    string(quoted, control)
}

#[cfg(test)]
mod tests;
