//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Core control, string, regex, and basic numeric built-ins.

use crate::{
    ast::BinaryOp,
    error::{Result, SQLError},
    expr::{eval_value_comparison_truth_with_engine, EvalContext},
};
use uqa_core::{
    memory::{Produced, ProductionControl},
    Value,
};

mod numeric;
mod text;

pub(super) fn eval_core_functions(name: &str, args: &[Value]) -> Option<Result<Value>> {
    match name {
        "concat" | "concat_ws" => Some(concat(name, args)),
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

/// Whether a built-in call by this name reads its input collation without a registered catalog signature that would bind it. Analysis keeps such a call bound by name, so its selection reaches execution; the case-mapping functions and pattern operators already keep their selected bindings.
pub(crate) fn binds_input_collation_by_name(name: &str) -> bool {
    matches!(
        name.strip_prefix("pg_catalog.").unwrap_or(name),
        "starts_with"
            | "regexp_count"
            | "regexp_instr"
            | "regexp_like"
            | "regexp_match"
            | "regexp_matches"
            | "regexp_replace"
            | "regexp_split_to_array"
            | "regexp_split_to_table"
            | "regexp_substr"
    )
}

/// Built-ins whose results or diagnostics depend on the call's input collation (`PostgreSQL`'s `inputcollid`). `collation` is zero when analysis found the input collation indeterminate.
pub(super) fn eval_collated_functions_with_control(
    name: &str,
    args: &[Value],
    collation: u32,
    control: &ProductionControl<'_>,
) -> Option<Result<Produced<Value>>> {
    if matches!(
        name,
        "upper" | "lower" | "casefold" | "initcap" | "like" | "ilike" | "starts_with"
    ) {
        return Some(
            control
                .check()
                .map_err(SQLError::from)
                .and_then(|()| text::evaluate_collated(name, args, collation, control)),
        );
    }
    super::regex::evaluate(name, args, collation, control)
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
        "length" | "char_length" | "character_length" | "octet_length" | "trim" | "btrim"
        | "ltrim" | "rtrim" | "reverse" | "concat_op" | "replace" | "substring" | "substr"
        | "left" | "right" | "starts_with" | "position" | "strpos" | "ascii" | "chr"
        | "like_escape" | "similar_to_escape" => text::evaluate,
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
    selection_with_context(name, args, control, &EvalContext::new(None, &[]))
}

pub(super) fn selection_with_context(
    name: &str,
    args: &[Value],
    control: &ProductionControl<'_>,
    context: &EvalContext<'_>,
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
            if eval_value_comparison_truth_with_engine(
                BinaryOp::Equal,
                &args[0],
                &args[1],
                control,
                context.engine,
                None,
            )? == Some(true)
            {
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
                        let op = if name == "greatest" {
                            BinaryOp::Less
                        } else {
                            BinaryOp::Greater
                        };
                        if eval_value_comparison_truth_with_engine(
                            op,
                            previous,
                            value,
                            control,
                            context.engine,
                            context.enum_comparison_state(),
                        )? == Some(true)
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
                output.push_str(&super::value_to_string(value)?);
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
    let separator = super::value_to_string(separator)?;
    let parts: Vec<String> = args[1..]
        .iter()
        .filter(|value| !matches!(value, Value::Null))
        .map(super::value_to_string)
        .collect::<Result<_>>()?;
    Ok(Value::Str(parts.join(&separator)))
}

#[cfg(test)]
mod tests;
