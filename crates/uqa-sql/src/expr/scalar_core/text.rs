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
        datums::binary_payload,
        out_of_range,
        scalar_helpers::{casing::CaseMapping, CompiledLikePattern},
    },
};
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionString, ProductionVec},
    Value,
};

/// Text operations under the call's input collation, `PostgreSQL`'s `inputcollid`.
pub(super) fn evaluate_collated(
    name: &str,
    args: &[Value],
    collation: u32,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    match name {
        "upper" | "lower" | "casefold" | "initcap" => case_mapping(name, args, collation, control),
        "like" | "ilike" => like(args, name == "ilike", collation, control),
        "starts_with" => starts_with(args, collation, control),
        _ => unreachable!("collated text family membership was checked"),
    }
}

pub(super) fn evaluate(
    name: &str,
    args: &[Value],
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    match name {
        "reverse" => unary(name, args, control),
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
        "starts_with" => starts_with(
            args,
            crate::catalog::collations::DEFAULT_COLLATION_OID as u32,
            control,
        ),
        "position" | "strpos" => search(args, control),
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
        "like_escape" | "similar_to_escape" => {
            crate::expr::pattern::escape(name == "similar_to_escape", args, control)
        }
        _ => unreachable!("text family membership was checked"),
    }
}

fn unary(name: &str, args: &[Value], control: &ProductionControl<'_>) -> Result<Produced<Value>> {
    debug_assert_eq!(name, "reverse");
    if let [value] = args {
        if let Some(payload) = binary_payload(value, control)? {
            let mut output = ProductionVec::new(*control);
            for byte in payload.bytes().iter().rev() {
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
    let mut output = ProductionString::new(*control);
    for character in source.chars().rev() {
        output.push(character)?;
    }
    string(output.finish()?, control)
}

/// `lower`, `upper`, `casefold` and `initcap` are strict; a non-NULL input selects its provider from the input collation, so an indeterminate collation is reported only for a value the function maps. `casefold` reports `lower()` in that diagnostic, as `str_casefold` does.
fn case_mapping(
    name: &str,
    args: &[Value],
    collation: u32,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    let Some(value) = args.first() else {
        return Err(SQLError::TypeMismatch("string fn needs 1 arg".into()));
    };
    if matches!(value, Value::Null) {
        return plain(Value::Null, control);
    }
    let source = value_to_string_with_control(value, control)?;
    let subject = match name {
        "upper" => "upper() function",
        "initcap" => "initcap() function",
        _ => "lower() function",
    };
    let mapping = CaseMapping::select(collation, subject)?;
    let output = match name {
        "upper" => mapping.uppercase(&source, control)?,
        "lower" => mapping.lowercase(&source, control)?,
        "casefold" => mapping.fold(&source, control)?,
        "initcap" => mapping.titlecase(&source, control)?,
        _ => unreachable!("case-mapping family"),
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
    if bytes {
        if let Some(length) = crate::expr::datums::binary_length(value, control)? {
            return plain(Value::Int(length), control);
        }
    }
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
    // `bytea || bytea` joins the bytes; analysis reads an `unknown` operand as `bytea`, while `bytea || text` is `anytextcat`, which concatenates the output texts.
    if args.iter().all(|arg| {
        matches!(arg, Value::Bytes(_))
            || matches!(arg, Value::Datum(datum) if datum.type_oid() == 17)
    }) {
        let mut output = ProductionVec::new(*control);
        for value in args {
            let payload = binary_payload(value, control)?.expect("every argument is bytea");
            let bytes = payload.bytes();
            output.reserve(bytes.len())?;
            for byte in bytes {
                output.push_copy(*byte)?;
            }
        }
        let (output, memory) = output.finish()?.into_parts();
        return Ok(control.finish(Value::Bytes(output), memory)?);
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
    let start = to_i64_with_control(&args[1], control)?;
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
    if end < 1 {
        return if matches!(&args[0], Value::Bytes(_))
            || matches!(&args[0], Value::Datum(datum) if datum.type_oid() == 17)
        {
            plain(Value::Bytes(Vec::new()), control)
        } else {
            string(control.copy_text("")?, control)
        };
    }
    let binary = binary_payload(&args[0], control)?;
    let source = if binary.is_none() {
        Some(value_to_string_with_control(&args[0], control)?)
    } else {
        None
    };
    let count = match &binary {
        Some(payload) => payload.bytes().len(),
        None => character_count(source.as_ref().expect("text substring"), control)?,
    } as i64;
    let first = start.max(1).min(count + 1);
    let last = end.clamp(1, count + 1);
    if let Some(payload) = binary {
        let mut output = ProductionVec::new(*control);
        if last > first {
            for byte in &payload.bytes()[(first - 1) as usize..(last - 1) as usize] {
                output.push_copy(*byte)?;
            }
        }
        let (output, memory) = output.finish()?.into_parts();
        return Ok(control.finish(Value::Bytes(output), memory)?);
    }
    let source = source.expect("text substring");
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

fn starts_with(
    args: &[Value],
    collation: u32,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    let [source, prefix] = args else {
        return Err(SQLError::TypeMismatch("starts_with takes 2 args".into()));
    };
    if matches!(source, Value::Null) || matches!(prefix, Value::Null) {
        return plain(Value::Null, control);
    }
    // Every admitted built-in collation is deterministic. Its identity must still be valid before even an empty or longer prefix returns.
    crate::expr::collation::string_comparison_order(collation)?;
    let compare = |source: &[u8], prefix: &[u8]| -> Result<Produced<Value>> {
        let matches = match source.get(..prefix.len()) {
            Some(head) => uqa_core::compare_bytes_with_control(head, prefix, control)?.is_eq(),
            None => false,
        };
        plain(Value::Bool(matches), control)
    };
    if let (Value::Str(source), Value::Str(prefix)) = (source, prefix) {
        return compare(source.as_bytes(), prefix.as_bytes());
    }
    if let (Some(source), Some(prefix)) = (
        binary_payload(source, control)?,
        binary_payload(prefix, control)?,
    ) {
        return compare(source.bytes(), prefix.bytes());
    }
    let source = value_to_string_with_control(source, control)?;
    let prefix = value_to_string_with_control(prefix, control)?;
    compare(source.as_bytes(), prefix.as_bytes())
}

fn search(args: &[Value], control: &ProductionControl<'_>) -> Result<Produced<Value>> {
    if args.len() != 2 {
        return Err(SQLError::TypeMismatch("position takes 2 args".into()));
    }
    if args.iter().any(|value| matches!(value, Value::Null)) {
        return plain(Value::Null, control);
    }
    if let (Some(source), Some(needle)) = (
        binary_payload(&args[0], control)?,
        binary_payload(&args[1], control)?,
    ) {
        let source = source.bytes();
        let needle = needle.bytes();
        let mut position = 0;
        if needle.is_empty() {
            position = 1;
        } else {
            for (index, window) in source.windows(needle.len()).enumerate() {
                control.check()?;
                if window == needle {
                    position = index as i64 + 1;
                    break;
                }
            }
        }
        return plain(Value::Int(position), control);
    }
    let source = value_to_string_with_control(&args[0], control)?;
    let needle = value_to_string_with_control(&args[1], control)?;
    control.check()?;
    let position = if needle.is_empty() {
        1
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
    collation: u32,
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
    if let Some(escape) = args.get(2) {
        // The escape rewrite runs before the strict operator, so its diagnostic precedes a NULL input.
        CompiledLikePattern::validate_escape(&args[1], escape, control)?;
    }
    if matches!(args[0], Value::Null) {
        return plain(Value::Null, control);
    }
    let pattern = CompiledLikePattern::from_value_with_escape_with_control(
        &args[1],
        insensitive,
        args.get(2),
        collation,
        control,
    )?;
    plain(
        Value::Bool(pattern.try_matches_value_with_control(&args[0], control)?),
        control,
    )
}
