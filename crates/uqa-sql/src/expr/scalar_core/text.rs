//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{plain, string};
use crate::{
    error::{Result, SQLError},
    expr::{
        conversion::{to_i64_with_control, value_to_string_with_control},
        out_of_range,
        scalar_helpers::{casing, CompiledLikePattern},
    },
};
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionString, ProductionVec},
    Value,
};

pub(super) fn evaluate(
    name: &str,
    args: &[Value],
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    match name {
        "upper" | "lower" | "casefold" | "initcap" | "reverse" => unary(name, args, control),
        "length" | "char_length" | "character_length" => {
            length(args, name == "length", false, control)
        }
        "octet_length" => length(args, true, true, control),
        "trim" | "btrim" | "ltrim" | "rtrim" => {
            trim(args, name != "rtrim", name != "ltrim", control)
        }
        "concat_op" => concat(args, control),
        "replace" => replace(args, control),
        "substring" | "substr" => substring(args, control),
        "left" | "right" => left_or_right(args, name == "right", control),
        "starts_with" | "position" | "strpos" => search(name, args, control),
        "ascii" => {
            let source = value_to_string_with_control(&args[0], control)?;
            plain(
                Value::Int(
                    source
                        .chars()
                        .next()
                        .map_or(0, |character| i64::from(u32::from(character))),
                ),
                control,
            )
        }
        "chr" => {
            let value = to_i64_with_control(&args[0], control)?;
            let character = u32::try_from(value)
                .ok()
                .and_then(char::from_u32)
                .ok_or_else(|| {
                    SQLError::TypeMismatch(format!("chr: invalid code point {value}"))
                })?;
            let mut output = ProductionString::new(*control);
            output.push(character)?;
            string(output.finish()?, control)
        }
        "like" | "ilike" => like(args, name == "ilike", control),
        _ => unreachable!("text family membership was checked"),
    }
}

fn unary(name: &str, args: &[Value], control: &ProductionControl<'_>) -> Result<Produced<Value>> {
    if name == "reverse" {
        if let [Value::Bytes(bytes)] = args {
            let mut output = ProductionVec::new(*control);
            for byte in bytes.iter().rev() {
                output.push_copy(*byte)?;
            }
            let (output, memory) = output.finish()?.into_parts();
            return Ok(control.finish(Value::Bytes(output), memory)?);
        }
    }
    let Some(value) = args.first() else {
        return Err(SQLError::TypeMismatch("string fn needs 1 arg".into()));
    };
    if matches!(value, Value::Null) {
        return plain(Value::Null, control);
    }
    let source = value_to_string_with_control(value, control)?;
    let output = match name {
        "upper" => casing::uppercase(&source, control)?,
        "lower" => casing::lowercase(&source, control)?,
        "casefold" => casing::casefold(&source, control)?,
        "initcap" => casing::initcap(&source, control)?,
        "reverse" => {
            let mut output = ProductionString::new(*control);
            for character in source.chars().rev() {
                output.push(character)?;
            }
            output.finish()?
        }
        _ => unreachable!("unary text family"),
    };
    string(output, control)
}

fn character_count(text: &str, control: &ProductionControl<'_>) -> Result<usize> {
    let mut count = 0;
    for _ in text.chars() {
        control.check()?;
        count += 1;
    }
    Ok(count)
}

fn length(
    args: &[Value],
    bytes: bool,
    octets: bool,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    let [value] = args else {
        return Err(SQLError::TypeMismatch(
            if octets {
                "octet_length takes 1 arg"
            } else {
                "length takes 1 arg"
            }
            .into(),
        ));
    };
    let length = match value {
        Value::Null => return plain(Value::Null, control),
        Value::Str(text) => {
            if octets {
                text.len()
            } else {
                character_count(text, control)?
            }
        }
        Value::FixedChar(text) => {
            if octets {
                text.len()
            } else {
                character_count(text.trim_end_matches(' '), control)?
            }
        }
        Value::Bytes(bytes_value) if bytes => bytes_value.len(),
        _ => {
            return Err(SQLError::TypeMismatch(
                if octets {
                    "octet_length requires text, character, or bytea"
                } else {
                    "length requires text, character, or bytea"
                }
                .into(),
            ));
        }
    };
    plain(Value::Int(length as i64), control)
}

fn trim(
    args: &[Value],
    start: bool,
    end: bool,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    if args.is_empty() || args.len() > 2 {
        return Err(SQLError::TypeMismatch("trim takes 1-2 args".into()));
    }
    if args.iter().any(|arg| matches!(arg, Value::Null)) {
        return plain(Value::Null, control);
    }
    let source = value_to_string_with_control(&args[0], control)?;
    let set = args
        .get(1)
        .map(|value| value_to_string_with_control(value, control))
        .transpose()?;
    let matches = |character: char| -> Result<bool> {
        control.check()?;
        if let Some(set) = &set {
            for candidate in set.chars() {
                control.check()?;
                if character == candidate {
                    return Ok(true);
                }
            }
            Ok(false)
        } else {
            Ok(character.is_whitespace())
        }
    };
    let mut first = 0;
    let mut last = source.len();
    if start {
        for (index, character) in source.char_indices() {
            if !matches(character)? {
                break;
            }
            first = index + character.len_utf8();
        }
    }
    if end {
        for (index, character) in source[first..].char_indices().rev() {
            if !matches(character)? {
                break;
            }
            last = first + index;
        }
    }
    string(control.copy_text(&source[first..last])?, control)
}

fn concat(args: &[Value], control: &ProductionControl<'_>) -> Result<Produced<Value>> {
    if let [left, right] = args {
        let array_function = match (left, right) {
            (Value::Array(_), Value::Array(_)) => Some("array_cat"),
            (Value::Array(_), Value::Null) => return Ok(control.copy_value(left)?),
            (Value::Null, Value::Array(_)) => return Ok(control.copy_value(right)?),
            (Value::Array(_), _) => Some("array_append"),
            (_, Value::Array(_)) => Some("array_prepend"),
            _ => None,
        };
        if let Some(name) = array_function {
            return crate::expr::scalar_array::eval_array_functions_with_control(
                name, args, control,
            )
            .expect("registered array concatenation function");
        }
    }
    if args.iter().any(|arg| matches!(arg, Value::Null)) {
        return plain(Value::Null, control);
    }
    if let Some(value) = crate::expr::json::json_concat_with_control(args, control)? {
        return Ok(value);
    }
    let mut output = ProductionString::new(*control);
    for value in args {
        output.push_str(&value_to_string_with_control(value, control)?)?;
    }
    string(output.finish()?, control)
}

fn replace(args: &[Value], control: &ProductionControl<'_>) -> Result<Produced<Value>> {
    if args.len() != 3 {
        return Err(SQLError::TypeMismatch("replace takes 3 args".into()));
    }
    if args.iter().any(|arg| matches!(arg, Value::Null)) {
        return plain(Value::Null, control);
    }
    let source = value_to_string_with_control(&args[0], control)?;
    let from = value_to_string_with_control(&args[1], control)?;
    let to = value_to_string_with_control(&args[2], control)?;
    let mut output = ProductionString::new(*control);
    let mut previous = 0;
    for (index, matched) in source.match_indices(from.as_str()) {
        control.check()?;
        output.push_str(&source[previous..index])?;
        output.push_str(&to)?;
        previous = index + matched.len();
    }
    output.push_str(&source[previous..])?;
    string(output.finish()?, control)
}

fn substring(args: &[Value], control: &ProductionControl<'_>) -> Result<Produced<Value>> {
    if !(2..=3).contains(&args.len()) {
        return Err(SQLError::TypeMismatch("substring takes 2-3 args".into()));
    }
    if args.iter().any(|arg| matches!(arg, Value::Null)) {
        return plain(Value::Null, control);
    }
    let source = value_to_string_with_control(&args[0], control)?;
    let start = to_i64_with_control(&args[1], control)?;
    let count = character_count(&source, control)? as i64;
    let end = if let Some(length) = args.get(2) {
        let length = to_i64_with_control(length, control)?;
        if length < 0 {
            return Err(SQLError::Routine {
                sqlstate: "22011".into(),
                message: "negative substring length not allowed".into(),
            });
        }
        start
            .checked_add(length)
            .ok_or_else(|| out_of_range("bigint"))?
    } else {
        i64::MAX
    };
    let first = start.max(1).min(count + 1);
    let last = end.clamp(1, count + 1);
    if last <= first {
        return string(control.copy_text("")?, control);
    }
    slice(&source, (first - 1) as usize, (last - 1) as usize, control)
}

fn left_or_right(
    args: &[Value],
    right: bool,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    if args.len() != 2 {
        return Err(SQLError::TypeMismatch(
            if right {
                "right takes 2 args"
            } else {
                "left takes 2 args"
            }
            .into(),
        ));
    }
    if args.iter().any(|arg| matches!(arg, Value::Null)) {
        return plain(Value::Null, control);
    }
    let source = value_to_string_with_control(&args[0], control)?;
    let requested = to_i64_with_control(&args[1], control)?;
    let count = character_count(&source, control)? as i64;
    let take = if requested >= 0 {
        requested.min(count)
    } else {
        (count + requested).max(0)
    } as usize;
    let first = if right { count as usize - take } else { 0 };
    slice(&source, first, first + take, control)
}

fn slice(
    source: &str,
    first: usize,
    last: usize,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    let mut start = source.len();
    let mut end = source.len();
    for (ordinal, (index, _)) in source.char_indices().enumerate() {
        control.check()?;
        if ordinal == first {
            start = index;
        }
        if ordinal == last {
            end = index;
            break;
        }
    }
    string(control.copy_text(&source[start..end])?, control)
}

fn search(name: &str, args: &[Value], control: &ProductionControl<'_>) -> Result<Produced<Value>> {
    if args.len() != 2 {
        return Err(SQLError::TypeMismatch(
            if name == "starts_with" {
                "starts_with takes 2 args"
            } else {
                "position takes 2 args"
            }
            .into(),
        ));
    }
    let source = value_to_string_with_control(&args[0], control)?;
    let needle = value_to_string_with_control(&args[1], control)?;
    control.check()?;
    if name == "starts_with" {
        return plain(Value::Bool(source.starts_with(needle.as_str())), control);
    }
    let position = if needle.is_empty() {
        0
    } else if let Some(index) = source.find(needle.as_str()) {
        character_count(&source[..index], control)? as i64 + 1
    } else {
        0
    };
    plain(Value::Int(position), control)
}

fn like(
    args: &[Value],
    insensitive: bool,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    if !matches!(args.len(), 2 | 3) {
        return Err(SQLError::TypeMismatch(
            if insensitive {
                "ILIKE takes 2 or 3 args"
            } else {
                "LIKE takes 2 or 3 args"
            }
            .into(),
        ));
    }
    if matches!(args[1], Value::Null) || matches!(args.get(2), Some(Value::Null)) {
        return plain(Value::Null, control);
    }
    let escape = args
        .get(2)
        .map(|value| value_to_string_with_control(value, control))
        .transpose()?;
    let source = value_to_string_with_control(&args[1], control)?;
    let pattern = CompiledLikePattern::with_escape_with_control(
        &source,
        insensitive,
        escape.as_deref().map(String::as_str),
        control,
    )?;
    if matches!(args[0], Value::Null) {
        return plain(Value::Null, control);
    }
    plain(
        Value::Bool(pattern.try_matches_value_with_control(&args[0], control)?),
        control,
    )
}
