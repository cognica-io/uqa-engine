//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Core control, string, regex, and basic numeric built-ins.

use crate::{
    error::{Result, SQLError},
    expr::{compare_with_control, value_to_string, values_equal_with_control},
};
use uqa_core::{
    memory::{Produced, ProductionControl},
    Value,
};

mod numeric;
mod regex;
mod text;

pub(super) fn eval_core_functions(name: &str, args: &[Value]) -> Option<Result<Value>> {
    match name {
        "concat" | "concat_ws" => Some(concat(name, args)),
        "regexp_match" | "regexp_matches" | "regexp_replace" => Some(regex::evaluate(name, args)),
        _ => eval_core_functions_with_control(name, args, &ProductionControl::uncontrolled()).map(
            |result| {
                result.map(|value| {
                    value
                        .into_uncontrolled()
                        .expect("ordinary core function has no retained owner")
                })
            },
        ),
    }
}

pub(super) fn eval_core_functions_with_control(
    name: &str,
    args: &[Value],
    control: &ProductionControl<'_>,
) -> Option<Result<Produced<Value>>> {
    let evaluate = match name {
        "coalesce" | "nullif" | "greatest" | "least" => selection,
        "abs" | "round" | "ceil" | "ceiling" | "floor" | "power" | "pow" | "sqrt" | "mod"
        | "div" | "gcd" | "lcm" => numeric::evaluate,
        "upper" | "lower" | "casefold" | "length" | "char_length" | "character_length"
        | "octet_length" | "trim" | "btrim" | "ltrim" | "rtrim" | "initcap" | "reverse"
        | "concat_op" | "replace" | "substring" | "substr" | "left" | "right" | "starts_with"
        | "position" | "strpos" | "ascii" | "like" | "ilike" | "chr" => text::evaluate,
        _ => return None,
    };
    Some(
        control
            .check()
            .map_err(SQLError::from)
            .and_then(|()| evaluate(name, args, control)),
    )
}

fn plain(value: Value, control: &ProductionControl<'_>) -> Result<Produced<Value>> {
    Ok(control.finish(value, control.empty_reservation())?)
}

fn string(value: Produced<String>, control: &ProductionControl<'_>) -> Result<Produced<Value>> {
    let (value, memory) = value.into_parts();
    Ok(control.finish(Value::Str(value), memory)?)
}

fn selection(
    name: &str,
    args: &[Value],
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    let selected = match name {
        "coalesce" => {
            let mut selected = &Value::Null;
            for value in args {
                control.check()?;
                if !matches!(value, Value::Null) {
                    selected = value;
                    break;
                }
            }
            selected
        }
        "nullif" => {
            if args.len() != 2 {
                return Err(SQLError::TypeMismatch("nullif takes 2 args".into()));
            }
            if values_equal_with_control(&args[0], &args[1], control)? {
                &Value::Null
            } else {
                &args[0]
            }
        }
        "greatest" | "least" => {
            let mut selected: Option<&Value> = None;
            for value in args {
                control.check()?;
                if matches!(value, Value::Null) {
                    continue;
                }
                selected = Some(match selected {
                    None => value,
                    Some(previous) => {
                        let order = compare_with_control(value, previous, control)?;
                        if (name == "greatest" && order.is_gt())
                            || (name == "least" && order.is_lt())
                        {
                            value
                        } else {
                            previous
                        }
                    }
                });
            }
            selected.unwrap_or(&Value::Null)
        }
        _ => unreachable!("selection family membership was checked"),
    };
    Ok(control.copy_value(selected)?)
}

fn concat(name: &str, args: &[Value]) -> Result<Value> {
    if name == "concat" {
        let mut output = String::new();
        for value in args {
            if !matches!(value, Value::Null) {
                output.push_str(&value_to_string(value));
            }
        }
        return Ok(Value::Str(output));
    }
    let Some(separator) = args.first() else {
        return Err(SQLError::TypeMismatch("concat_ws needs separator".into()));
    };
    if matches!(separator, Value::Null) {
        return Ok(Value::Null);
    }
    let separator = value_to_string(separator);
    let parts: Vec<String> = args[1..]
        .iter()
        .filter(|value| !matches!(value, Value::Null))
        .map(value_to_string)
        .collect();
    Ok(Value::Str(parts.join(&separator)))
}

#[cfg(test)]
mod tests;
