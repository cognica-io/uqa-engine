//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Capture, position and predicate results retain only admitted SQL values.

use super::{compile, flags, invalid_parameter, nth_capture, parameter, plain, string, tail};
use crate::error::{Result, SQLError};
use crate::expr::{conversion::value_to_string_with_control, out_of_range};
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    ArrayValue, Value,
};

pub(super) fn captures(
    name: &str,
    args: &[Value],
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    if !(2..=3).contains(&args.len()) {
        return Err(SQLError::TypeMismatch(
            "regexp_match takes 2 or 3 args".into(),
        ));
    }
    let input = value_to_string_with_control(&args[0], control)?;
    let pattern = value_to_string_with_control(&args[1], control)?;
    let flags = flags(args.get(2), control)?;
    let regex = compile(&pattern, &flags, name == "regexp_matches", control)?;
    let captures = regex.captures(&input);
    control.check()?;
    let Some(captures) = captures else {
        return plain(Value::Null, control);
    };
    let mut groups = ProductionVec::new(*control);
    if captures.len() == 1 {
        let matched = captures.get(0).ok_or_else(|| {
            SQLError::Internal("regex capture set omitted its mandatory full match".into())
        })?;
        groups.push_produced(string(control.copy_text(matched.as_str())?, control)?)?;
    } else {
        groups.reserve(captures.len() - 1)?;
        for matched in captures.iter().skip(1) {
            let value = match matched {
                Some(matched) => string(control.copy_text(matched.as_str())?, control)?,
                None => plain(Value::Null, control)?,
            };
            groups.push_produced(value)?;
        }
    }
    let array = ArrayValue::try_new_with_control(groups.finish()?, control)?
        .ok_or_else(|| SQLError::TypeMismatch("invalid regexp_match result".into()))?;
    let (array, memory) = array.into_parts();
    control
        .finish(Value::Array(array), memory)
        .map_err(Into::into)
}

pub(super) fn evaluate(
    name: &str,
    args: &[Value],
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    if name == "similar_to" {
        return similar(args, control);
    }
    let (maximum, message) = match name {
        "regexp_count" => (4, "regexp_count takes 2-4 args"),
        "regexp_instr" => (7, "regexp_instr takes 2-7 args"),
        "regexp_like" => (3, "regexp_like takes 2-3 args"),
        "regexp_substr" => (6, "regexp_substr takes 2-6 args"),
        _ => unreachable!("regular-expression family membership was checked"),
    };
    if !(2..=maximum).contains(&args.len()) {
        return Err(SQLError::TypeMismatch(message.into()));
    }
    if args.iter().any(|value| matches!(value, Value::Null)) {
        return plain(Value::Null, control);
    }
    let input = value_to_string_with_control(&args[0], control)?;
    let pattern = value_to_string_with_control(&args[1], control)?;
    match name {
        "regexp_count" => count(&input, &pattern, args, control),
        "regexp_instr" | "regexp_substr" => selected(name, &input, &pattern, args, control),
        "regexp_like" => {
            let flags = flags(args.get(2), control)?;
            let regex = compile(&pattern, &flags, false, control)?;
            plain(Value::Bool(regex.is_match(&input)), control)
        }
        _ => unreachable!("regular-expression family membership was checked"),
    }
}

fn count(
    input: &str,
    pattern: &str,
    args: &[Value],
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    let start = parameter(args.get(2), 1, 1, "start", control)?;
    let flags = flags(args.get(3), control)?;
    let regex = compile(pattern, &flags, false, control)?;
    let Some((tail, _)) = tail(input, start, control)? else {
        return plain(Value::Int(0), control);
    };
    let mut count = 0_i64;
    for _ in regex.find_iter(tail) {
        control.check()?;
        count = count
            .checked_add(1)
            .ok_or_else(|| out_of_range("integer"))?;
    }
    plain(Value::Int(count), control)
}

fn selected(
    name: &str,
    input: &str,
    pattern: &str,
    args: &[Value],
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    let start = parameter(args.get(2), 1, 1, "start", control)?;
    let occurrence = parameter(args.get(3), 1, 1, "N", control)?;
    let position = name == "regexp_instr";
    let end_option = if position {
        let value = args
            .get(4)
            .map(|value| crate::expr::conversion::to_i64_with_control(value, control))
            .transpose()?
            .unwrap_or(0);
        if !matches!(value, 0 | 1) {
            return Err(invalid_parameter("endoption", value));
        }
        value
    } else {
        0
    };
    let flags = flags(args.get(if position { 5 } else { 4 }), control)?;
    let group = parameter(
        args.get(if position { 6 } else { 5 }),
        0,
        0,
        "subexpr",
        control,
    )?;
    let regex = compile(pattern, &flags, false, control)?;
    let absent = || plain(if position { Value::Int(0) } else { Value::Null }, control);
    let Some((tail, base_chars)) = tail(input, start, control)? else {
        return absent();
    };
    let Some(captures) = nth_capture(&regex, tail, occurrence, control)? else {
        return absent();
    };
    let Some(matched) = captures.get(group) else {
        return absent();
    };
    if !position {
        return string(control.copy_text(matched.as_str())?, control);
    }
    let byte_offset = if end_option == 0 {
        matched.start()
    } else {
        matched.end()
    };
    let mut position = base_chars;
    for _ in tail[..byte_offset].chars() {
        control.check()?;
        position = position
            .checked_add(1)
            .ok_or_else(|| out_of_range("integer"))?;
    }
    let position = position
        .checked_add(1)
        .and_then(|value| i64::try_from(value).ok())
        .ok_or_else(|| out_of_range("integer"))?;
    plain(Value::Int(position), control)
}

fn similar(args: &[Value], control: &ProductionControl<'_>) -> Result<Produced<Value>> {
    if !matches!(args.len(), 2 | 3) {
        return Err(SQLError::TypeMismatch(
            "similar_to takes 2 or 3 args".into(),
        ));
    }
    if matches!(args[1], Value::Null) || matches!(args.get(2), Some(Value::Null)) {
        return plain(Value::Null, control);
    }
    let escape = args
        .get(2)
        .map(|value| value_to_string_with_control(value, control))
        .transpose()?;
    let input_pattern = value_to_string_with_control(&args[1], control)?;
    let pattern = crate::expr::scalar_helpers::similar_to_regex_with_control(
        &input_pattern,
        escape.as_ref().map(|value| value.as_str()),
        control,
    )?;
    if matches!(args[0], Value::Null) {
        return plain(Value::Null, control);
    }
    let input = value_to_string_with_control(&args[0], control)?;
    let regex = compile(&pattern, "", false, control)?;
    plain(Value::Bool(regex.is_match(&input)), control)
}
