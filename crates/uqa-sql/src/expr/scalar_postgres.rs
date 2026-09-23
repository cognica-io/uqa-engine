//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Extended `PostgreSQL` scalar and lowered operator built-ins.

use super::{
    compile_pg_regex, eval_between, eval_comparison_op, expect_str, out_of_range, quote_ident,
    quote_literal, similar_to_regex, to_i64, value_to_string, values_equal, ArrayValue, BinaryOp,
    DecimalValue, Result, SQLError, Value,
};
use crate::ast::FunctionDispatch;

mod arrays;
mod subscripts;
pub(super) use subscripts::eval_postgres_subscript_with_control;
mod text;
pub(super) use arrays::eval_postgres_arrays_with_control;
use uqa_core::memory::ProductionControl;

use text::{
    invalid_regex_parameter, nonnegative_regex_parameter, positive_regex_parameter, regex_tail,
};

pub(super) fn eval_postgres_functions(name: &str, args: &[Value]) -> Option<Result<Value>> {
    if let Some(result) =
        eval_postgres_arrays_with_control(name, args, &ProductionControl::uncontrolled())
    {
        return Some(result.map(|value| {
            value
                .into_uncontrolled()
                .expect("ordinary postgres array result")
        }));
    }
    const NAMES: &[&str] = &[
        "factorial",
        "bit_length",
        "to_bin",
        "to_hex",
        "to_oct",
        "string_to_array",
        "string_to_table",
        "quote_ident",
        "quote_literal",
        "quote_nullable",
        "regexp_count",
        "regexp_instr",
        "regexp_like",
        "regexp_substr",
        "similar_to",
        "num_nulls",
        "num_nonnulls",
        "current_database",
        "version",
        "current_catalog",
        "current_user",
        "session_user",
        "array_sample",
    ];
    if !NAMES.contains(&name) {
        return None;
    }
    Some(eval_postgres_function(name, args))
}

pub(super) fn eval_dispatched_postgres_function(
    dispatch: FunctionDispatch,
    args: &[Value],
) -> Option<Result<Value>> {
    if let Some(result) =
        eval_postgres_subscript_with_control(dispatch, args, &ProductionControl::uncontrolled())
    {
        return Some(result.map(|value| {
            value
                .into_uncontrolled()
                .expect("ordinary subscript result")
        }));
    }
    Some(match dispatch {
        FunctionDispatch::AnyOperator => eval_any_all(args, true),
        FunctionDispatch::AllOperator => eval_any_all(args, false),
        FunctionDispatch::IsDistinct => eval_is_distinct(args),
        FunctionDispatch::BetweenSymmetric => eval_between_symmetric(args),
        FunctionDispatch::ToBinInt4
        | FunctionDispatch::ToBinInt8
        | FunctionDispatch::ToHexInt4
        | FunctionDispatch::ToHexInt8
        | FunctionDispatch::ToOctInt4
        | FunctionDispatch::ToOctInt8 => eval_integer_base(dispatch, args),
        _ => return None,
    })
}

#[expect(
    clippy::too_many_lines,
    reason = "builtin dispatch preserves arity, NULL, and error precedence"
)]
fn eval_postgres_function(name: &str, args: &[Value]) -> Result<Value> {
    (|| -> Result<Value> {
        match name {
            // -------------------------------------------------------------
            // PostgreSQL scalar surface: math, strings, arrays, operators
            // lowered to internal functions.
            // -------------------------------------------------------------
            "factorial" => {
                if args.len() != 1 {
                    return Err(SQLError::TypeMismatch("factorial takes 1 arg".into()));
                }
                if matches!(args[0], Value::Null) {
                    return Ok(Value::Null);
                }
                let n = to_i64(&args[0])?;
                if n < 0 {
                    return Err(SQLError::Routine {
                        sqlstate: "2201F".into(),
                        message: "factorial of a negative number is undefined".into(),
                    });
                }
                let mut acc: i128 = 1;
                for k in 2..=n as i128 {
                    acc = acc.checked_mul(k).ok_or_else(|| out_of_range("numeric"))?;
                }
                if let Ok(small) = i64::try_from(acc) {
                    return Ok(Value::Int(small));
                }
                DecimalValue::parse(&acc.to_string())
                    .map(Value::Decimal)
                    .ok_or_else(|| out_of_range("numeric"))
            }
            "bit_length" => {
                let [value] = args else {
                    return Err(SQLError::TypeMismatch("bit_length takes 1 arg".into()));
                };
                let octets = match value {
                    Value::Null => return Ok(Value::Null),
                    Value::Str(text) => text.len(),
                    Value::FixedChar(text) => text.trim_end_matches(' ').len(),
                    Value::Bytes(bytes) => bytes.len(),
                    _ => {
                        return Err(SQLError::TypeMismatch(
                            "bit_length requires text or bytea".into(),
                        ));
                    }
                };
                Ok(Value::Int(octets as i64 * 8))
            }
            "to_bin" | "to_hex" | "to_oct" => Err(SQLError::Internal(format!(
                "{name} reached runtime before its integer overload was bound"
            ))),
            "string_to_array" | "string_to_table" => {
                if args.len() < 2 || args.len() > 3 {
                    return Err(SQLError::TypeMismatch(
                        "string_to_array takes 2-3 args".into(),
                    ));
                }
                if matches!(args[0], Value::Null) {
                    return Ok(Value::Null);
                }
                let s = value_to_string(&args[0]);
                let null_marker = args.get(2).filter(|v| !matches!(v, Value::Null));
                let mark = |part: &str| -> Value {
                    if let Some(marker) = null_marker {
                        if part == value_to_string(marker) {
                            return Value::Null;
                        }
                    }
                    Value::Str(part.to_string())
                };
                let items: Vec<Value> = match &args[1] {
                    // NULL separator: split into individual characters.
                    Value::Null => s.chars().map(|c| mark(&c.to_string())).collect(),
                    sep => {
                        let sep = value_to_string(sep);
                        if s.is_empty() {
                            Vec::new()
                        } else if sep.is_empty() {
                            vec![mark(&s)]
                        } else {
                            s.split(sep.as_str()).map(mark).collect()
                        }
                    }
                };
                ArrayValue::try_new(items)
                    .map(Value::Array)
                    .ok_or_else(|| SQLError::TypeMismatch("invalid string_to_array result".into()))
            }
            "quote_ident" => {
                if matches!(args.first(), Some(Value::Null)) {
                    return Ok(Value::Null);
                }
                Ok(Value::Str(quote_ident(&expect_str(args, 0)?)))
            }
            "quote_literal" => {
                if matches!(args.first(), Some(Value::Null)) {
                    return Ok(Value::Null);
                }
                Ok(Value::Str(quote_literal(&expect_str(args, 0)?)))
            }
            "quote_nullable" => match args.first() {
                Some(Value::Null) | None => Ok(Value::Str("NULL".into())),
                Some(other) => Ok(Value::Str(quote_literal(&value_to_string(other)))),
            },
            "regexp_count" => {
                if args.len() < 2 || args.len() > 4 {
                    return Err(SQLError::TypeMismatch("regexp_count takes 2-4 args".into()));
                }
                if args.iter().any(|arg| matches!(arg, Value::Null)) {
                    return Ok(Value::Null);
                }
                let s = value_to_string(&args[0]);
                let pat = value_to_string(&args[1]);
                let start = positive_regex_parameter(args.get(2), 1, "start")?;
                let flags = args.get(3).map(value_to_string).unwrap_or_default();
                let re = compile_pg_regex(&pat, &flags, false)?;
                let Some((tail, _)) = regex_tail(&s, start) else {
                    return Ok(Value::Int(0));
                };
                Ok(Value::Int(re.find_iter(tail).count() as i64))
            }
            "regexp_instr" => {
                if args.len() < 2 || args.len() > 7 {
                    return Err(SQLError::TypeMismatch("regexp_instr takes 2-7 args".into()));
                }
                if args.iter().any(|arg| matches!(arg, Value::Null)) {
                    return Ok(Value::Null);
                }
                let string = value_to_string(&args[0]);
                let pattern = value_to_string(&args[1]);
                let start = positive_regex_parameter(args.get(2), 1, "start")?;
                let occurrence = positive_regex_parameter(args.get(3), 1, "N")?;
                let end_option = args.get(4).map(to_i64).transpose()?.unwrap_or(0);
                if !matches!(end_option, 0 | 1) {
                    return Err(invalid_regex_parameter("endoption", end_option));
                }
                let flags = args.get(5).map(value_to_string).unwrap_or_default();
                let subexpression = nonnegative_regex_parameter(args.get(6), 0, "subexpr")?;
                let re = compile_pg_regex(&pattern, &flags, false)?;
                let Some((tail, base_chars)) = regex_tail(&string, start) else {
                    return Ok(Value::Int(0));
                };
                let Some(captures) = re.captures_iter(tail).nth(occurrence - 1) else {
                    return Ok(Value::Int(0));
                };
                let Some(selected) = captures.get(subexpression) else {
                    return Ok(Value::Int(0));
                };
                let byte_offset = if end_option == 0 {
                    selected.start()
                } else {
                    selected.end()
                };
                let position = base_chars
                    .checked_add(tail[..byte_offset].chars().count())
                    .and_then(|position| position.checked_add(1))
                    .ok_or_else(|| out_of_range("integer"))?;
                Ok(Value::Int(
                    i64::try_from(position).map_err(|_| out_of_range("integer"))?,
                ))
            }
            "regexp_like" => {
                if args.len() < 2 || args.len() > 3 {
                    return Err(SQLError::TypeMismatch("regexp_like takes 2-3 args".into()));
                }
                if args.iter().any(|arg| matches!(arg, Value::Null)) {
                    return Ok(Value::Null);
                }
                let s = value_to_string(&args[0]);
                let pat = value_to_string(&args[1]);
                let flags = args.get(2).map(value_to_string).unwrap_or_default();
                let re = compile_pg_regex(&pat, &flags, false)?;
                Ok(Value::Bool(re.is_match(&s)))
            }
            "regexp_substr" => {
                if args.len() < 2 || args.len() > 6 {
                    return Err(SQLError::TypeMismatch(
                        "regexp_substr takes 2-6 args".into(),
                    ));
                }
                if args.iter().any(|arg| matches!(arg, Value::Null)) {
                    return Ok(Value::Null);
                }
                let string = value_to_string(&args[0]);
                let pattern = value_to_string(&args[1]);
                let start = positive_regex_parameter(args.get(2), 1, "start")?;
                let occurrence = positive_regex_parameter(args.get(3), 1, "N")?;
                let flags = args.get(4).map(value_to_string).unwrap_or_default();
                let subexpression = nonnegative_regex_parameter(args.get(5), 0, "subexpr")?;
                let re = compile_pg_regex(&pattern, &flags, false)?;
                let Some((tail, _)) = regex_tail(&string, start) else {
                    return Ok(Value::Null);
                };
                let Some(captures) = re.captures_iter(tail).nth(occurrence - 1) else {
                    return Ok(Value::Null);
                };
                Ok(captures
                    .get(subexpression)
                    .map(|matched| Value::Str(matched.as_str().to_string()))
                    .unwrap_or(Value::Null))
            }
            "similar_to" => {
                // SIMILAR TO: SQL regex anchored over the whole string.
                if !matches!(args.len(), 2 | 3) {
                    return Err(SQLError::TypeMismatch(
                        "similar_to takes 2 or 3 args".into(),
                    ));
                }
                if matches!(args[1], Value::Null) || matches!(args.get(2), Some(Value::Null)) {
                    return Ok(Value::Null);
                }
                let escape = args.get(2).map(value_to_string);
                let pat = similar_to_regex(&value_to_string(&args[1]), escape.as_deref())?;
                if matches!(args[0], Value::Null) {
                    return Ok(Value::Null);
                }
                let s = value_to_string(&args[0]);
                let re = compile_pg_regex(&pat, "", false)?;
                Ok(Value::Bool(re.is_match(&s)))
            }
            "num_nulls" => Ok(Value::Int(
                args.iter().filter(|v| matches!(v, Value::Null)).count() as i64,
            )),
            "num_nonnulls" => Ok(Value::Int(
                args.iter().filter(|v| !matches!(v, Value::Null)).count() as i64,
            )),
            // The engine has one database and one logical user identity; schema
            // identifiers are intercepted above because they are session-scoped.
            "current_database" | "current_catalog" => Ok(Value::Str("uqa".into())),
            "version" => {
                if !args.is_empty() {
                    return Err(SQLError::BadArity {
                        name: name.into(),
                        expected: "0".into(),
                        actual: args.len(),
                    });
                }
                Ok(Value::Str(format!(
                    "UQA Engine {} on {}-{}, PostgreSQL 18 compatible",
                    env!("CARGO_PKG_VERSION"),
                    std::env::consts::ARCH,
                    std::env::consts::OS,
                )))
            }
            "current_user" | "session_user" => Ok(Value::Str("uqa".into())),
            "array_sample" => {
                if args.len() != 2 {
                    return Err(SQLError::TypeMismatch("array_sample takes 2 args".into()));
                }
                let Value::Array(array) = &args[0] else {
                    if matches!(args[0], Value::Null) {
                        return Ok(Value::Null);
                    }
                    return Err(SQLError::TypeMismatch("array_sample: not an array".into()));
                };
                let n = to_i64(&args[1])?;
                let n = usize::try_from(n).ok();
                if n.is_none_or(|n| n > array.elements().len()) {
                    return Err(SQLError::Routine {
                        sqlstate: "22023".into(),
                        message: format!(
                            "sample size must be between 0 and {}",
                            array.elements().len()
                        ),
                    });
                }
                let n = n.ok_or_else(|| out_of_range("array sample size"))?;
                let mut pool = array.elements().to_vec();
                let mut out = Vec::with_capacity(n);
                let mut seed = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.subsec_nanos() as u64 | 1)
                    .unwrap_or(1);
                for _ in 0..n {
                    seed = seed
                        .wrapping_mul(6_364_136_223_846_793_005)
                        .wrapping_add(1_442_695_040_888_963_407);
                    let idx = (seed >> 33) as usize % pool.len();
                    out.push(pool.swap_remove(idx));
                }
                let mut lower_bounds = array.lower_bounds().to_vec();
                if let Some(lower_bound) = lower_bounds.first_mut() {
                    *lower_bound = 1;
                }
                rebuild_array_with_bounds(out, lower_bounds)
            }
            _ => unreachable!("function family membership was checked before dispatch"),
        }
    })()
}

fn eval_integer_base(dispatch: FunctionDispatch, args: &[Value]) -> Result<Value> {
    let [argument] = args else {
        return Err(SQLError::TypeMismatch(format!(
            "{} takes 1 arg",
            dispatch.label()
        )));
    };
    if matches!(argument, Value::Null) {
        return Ok(Value::Null);
    }
    let value = to_i64(argument)?;
    Ok(Value::Str(match dispatch {
        FunctionDispatch::ToBinInt4 => {
            let value = i32::try_from(value).map_err(|_| out_of_range("integer"))?;
            format!("{:b}", value as u32)
        }
        FunctionDispatch::ToBinInt8 => format!("{:b}", value as u64),
        FunctionDispatch::ToHexInt4 => {
            let value = i32::try_from(value).map_err(|_| out_of_range("integer"))?;
            format!("{:x}", value as u32)
        }
        FunctionDispatch::ToHexInt8 => format!("{:x}", value as u64),
        FunctionDispatch::ToOctInt4 => {
            let value = i32::try_from(value).map_err(|_| out_of_range("integer"))?;
            format!("{:o}", value as u32)
        }
        FunctionDispatch::ToOctInt8 => format!("{:o}", value as u64),
        _ => unreachable!("integer-base dispatch was checked by the caller"),
    }))
}

fn eval_any_all(args: &[Value], is_any: bool) -> Result<Value> {
    if args.len() != 3 {
        return Err(SQLError::TypeMismatch("ANY/ALL takes 3 args".into()));
    }
    let op = match value_to_string(&args[2]).as_str() {
        "=" => BinaryOp::Equal,
        "<>" | "!=" => BinaryOp::NotEqual,
        "<" => BinaryOp::Less,
        "<=" => BinaryOp::LessEqual,
        ">" => BinaryOp::Greater,
        ">=" => BinaryOp::GreaterEqual,
        other => {
            return Err(SQLError::Unsupported(format!(
                "operator `{other}` with ANY/ALL"
            )));
        }
    };
    let Value::Array(array) = &args[1] else {
        if matches!(args[1], Value::Null) {
            return Ok(Value::Null);
        }
        return Err(SQLError::TypeMismatch("ANY/ALL requires an array".into()));
    };
    let mut saw_null = false;
    let mut items = Vec::new();
    flatten_array_elements(array.elements(), &mut items);
    for item in items {
        match eval_comparison_op(op, &args[0], item)? {
            Value::Bool(true) if is_any => return Ok(Value::Bool(true)),
            Value::Bool(false) if !is_any => return Ok(Value::Bool(false)),
            Value::Null => saw_null = true,
            _ => {}
        }
    }
    if saw_null {
        return Ok(Value::Null);
    }
    Ok(Value::Bool(!is_any))
}

fn eval_is_distinct(args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(SQLError::TypeMismatch(
            "IS DISTINCT FROM takes 2 args".into(),
        ));
    }
    let distinct = match (&args[0], &args[1]) {
        (Value::Null, Value::Null) => false,
        (Value::Null, _) | (_, Value::Null) => true,
        (left, right) => !values_equal(left, right),
    };
    Ok(Value::Bool(distinct))
}

fn eval_between_symmetric(args: &[Value]) -> Result<Value> {
    if args.len() != 3 {
        return Err(SQLError::TypeMismatch(
            "BETWEEN SYMMETRIC takes 3 args".into(),
        ));
    }
    let forward = eval_between(&args[0], &args[1], &args[2])?;
    let backward = eval_between(&args[0], &args[2], &args[1])?;
    Ok(match (&forward, &backward) {
        (Value::Bool(true), _) | (_, Value::Bool(true)) => Value::Bool(true),
        (Value::Null, _) | (_, Value::Null) => Value::Null,
        _ => Value::Bool(false),
    })
}

fn rebuild_array_with_bounds(elements: Vec<Value>, lower_bounds: Vec<i32>) -> Result<Value> {
    let rebuilt = if elements.is_empty() {
        ArrayValue::try_new(elements)
    } else {
        ArrayValue::with_lower_bounds(elements, lower_bounds)
    };
    rebuilt
        .map(Value::Array)
        .ok_or_else(|| SQLError::TypeMismatch("array dimensions do not match".into()))
}

fn flatten_array_elements<'a>(elements: &'a [Value], output: &mut Vec<&'a Value>) {
    for element in elements {
        if let Value::List(nested) = element {
            flatten_array_elements(nested, output);
        } else {
            output.push(element);
        }
    }
}
