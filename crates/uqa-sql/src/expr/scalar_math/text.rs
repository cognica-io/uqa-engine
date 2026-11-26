//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared text-producing math builtins retain output and intermediate buffers under their producer control.

use uqa_core::memory::{Produced, ProductionControl, ProductionString, ProductionVec};

use super::{plain, Result, SQLError, Value};
use crate::expr::{
    allocation_error,
    conversion::{
        argument_text, character_count, character_offsets, to_i64_with_control,
        value_to_string_with_control,
    },
    encoding::{
        base64_decode_with_control, base64_encode_with_control, escape_decode_with_control,
        escape_encode_with_control, hex_decode_with_control, hex_encode_with_control,
        md5_hex_with_control,
    },
    out_of_range,
};

#[cfg(test)]
mod limits;

use crate::expr::text_size::check_text_size;

fn text_count(value: &Value, control: &ProductionControl<'_>) -> Result<usize> {
    let value =
        i32::try_from(to_i64_with_control(value, control)?).map_err(|_| out_of_range("integer"))?;
    Ok(value.max(0) as usize)
}

fn string(value: Produced<String>, control: &ProductionControl<'_>) -> Result<Produced<Value>> {
    let (value, memory) = value.into_parts();
    Ok(control.finish(Value::Str(value), memory)?)
}

fn bytes(value: Produced<Vec<u8>>, control: &ProductionControl<'_>) -> Result<Produced<Value>> {
    let (value, memory) = value.into_parts();
    Ok(control.finish(Value::Bytes(value), memory)?)
}

fn characters(value: &str, control: &ProductionControl<'_>) -> Result<Produced<Vec<char>>> {
    let mut out = ProductionVec::new(*control);
    for character in value.chars() {
        out.push_copy(character)?;
    }
    Ok(out.finish()?)
}

#[expect(
    clippy::too_many_lines,
    reason = "builtin dispatch preserves arity, NULL, and conversion order"
)]
pub(super) fn eval(
    name: &str,
    args: &[Value],
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    match name {
        "lpad" | "rpad" => {
            if args.len() < 2 || args.len() > 3 {
                return Err(SQLError::TypeMismatch("[lr]pad takes 2-3 args".into()));
            }
            let s = value_to_string_with_control(&args[0], control)?;
            let n = text_count(&args[1], control)?;
            let fill = match args.get(2) {
                Some(value) => value_to_string_with_control(value, control)?,
                None => control.copy_text(" ")?,
            };
            let chars = characters(&s, control)?;
            // PostgreSQL checks the UTF-8 worst case, including when truncating. An empty fill limits the result to the input's character count.
            let result_chars = if fill.is_empty() {
                n.min(chars.len())
            } else {
                n
            };
            check_text_size(result_chars.checked_mul(4))?;
            let mut out = ProductionString::new(*control);
            if chars.len() >= n {
                for character in &chars[..n] {
                    out.push(*character)?;
                }
                return string(out.finish()?, control);
            }
            let need = n - chars.len();
            let fill_chars = characters(&fill, control)?;
            if fill_chars.is_empty() {
                return string(s, control);
            }
            let prefix_bytes = fill_chars[..need % fill_chars.len()]
                .iter()
                .try_fold(0_usize, |bytes, character| {
                    bytes.checked_add(character.len_utf8())
                })
                .ok_or_else(|| allocation_error("lpad/rpad"))?;
            let capacity = fill
                .len()
                .checked_mul(need / fill_chars.len())
                .and_then(|bytes| bytes.checked_add(prefix_bytes))
                .and_then(|bytes| bytes.checked_add(s.len()))
                .ok_or_else(|| allocation_error("lpad/rpad"))?;
            out.reserve(capacity)?;
            if name == "rpad" {
                out.push_str(&s)?;
            }
            for index in 0..need {
                out.push(fill_chars[index % fill_chars.len()])?;
            }
            if name == "lpad" {
                out.push_str(&s)?;
            }
            string(out.finish()?, control)
        }
        "repeat" => {
            if args.len() != 2 {
                return Err(SQLError::TypeMismatch("repeat takes 2 args".into()));
            }
            let s = value_to_string_with_control(&args[0], control)?;
            let n = text_count(&args[1], control)?;
            check_text_size(s.len().checked_mul(n))?;
            let mut out = ProductionString::new(*control);
            if n != 0 && !s.is_empty() {
                let capacity = s
                    .len()
                    .checked_mul(n)
                    .ok_or_else(|| allocation_error("repeat"))?;
                out.reserve(capacity)?;
                for _ in 0..n {
                    out.push_str(&s)?;
                }
            }
            string(out.finish()?, control)
        }
        "translate" => {
            if args.len() != 3 {
                return Err(SQLError::TypeMismatch("translate takes 3 args".into()));
            }
            let s = value_to_string_with_control(&args[0], control)?;
            check_text_size(s.len().checked_mul(4))?;
            let from = characters(&value_to_string_with_control(&args[1], control)?, control)?;
            let to = characters(&value_to_string_with_control(&args[2], control)?, control)?;
            let mut out = ProductionString::new(*control);
            for character in s.chars() {
                control.check()?;
                let mut index = None;
                for (position, candidate) in from.iter().enumerate() {
                    control.check()?;
                    if *candidate == character {
                        index = Some(position);
                        break;
                    }
                }
                match index {
                    Some(index) if index < to.len() => out.push(to[index])?,
                    Some(_) => (),
                    None => out.push(character)?,
                }
            }
            string(out.finish()?, control)
        }
        "overlay" => overlay(args, control),
        "md5" | "encode" => encode(name, args, control),
        "decode" => decode(args, control),
        "split_part" => split_part(args, control),
        _ => unreachable!("text family membership was checked before dispatch"),
    }
}

fn overlay(args: &[Value], control: &ProductionControl<'_>) -> Result<Produced<Value>> {
    if !(3..=4).contains(&args.len()) {
        return Err(SQLError::TypeMismatch("overlay takes 3 or 4 args".into()));
    }
    if args.iter().any(|value| matches!(value, Value::Null)) {
        return plain(Value::Null, control);
    }
    let text = argument_text(&args[0], control)?;
    let replacement = argument_text(&args[1], control)?;
    let integer = |value| {
        i32::try_from(to_i64_with_control(value, control)?).map_err(|_| out_of_range("integer"))
    };
    let start = integer(&args[2])?;
    if start <= 0 {
        return Err(SQLError::Routine {
            sqlstate: "22011".into(),
            message: "negative substring length not allowed".into(),
        });
    }
    let length = match args.get(3) {
        Some(value) => integer(value)?,
        None => i32::try_from(character_count(&replacement, control)?)
            .map_err(|_| out_of_range("integer"))?,
    };
    let end = start
        .checked_add(length)
        .ok_or_else(|| out_of_range("integer"))?;
    let (first, last) = character_offsets(
        &text,
        (start - 1) as usize,
        (end.max(1) - 1) as usize,
        control,
    )?;
    let prefix = &text[..first];
    let suffix = &text[last..];
    let replaced = prefix.len() + replacement.len();
    crate::expr::text_size::check_varlena_size(replaced)?;
    let total = replaced + suffix.len();
    crate::expr::text_size::check_varlena_size(total)?;
    let mut output = ProductionString::new(*control);
    output.reserve(total)?;
    output.push_str(prefix)?;
    output.push_str(&replacement)?;
    output.push_str(suffix)?;
    string(output.finish()?, control)
}

fn encode(name: &str, args: &[Value], control: &ProductionControl<'_>) -> Result<Produced<Value>> {
    let arity = if name == "md5" { 1 } else { 2 };
    if args.len() != arity {
        return Err(SQLError::TypeMismatch(
            if name == "md5" {
                "md5 takes 1 arg"
            } else {
                "encode takes 2 args"
            }
            .into(),
        ));
    }
    let owned;
    let binary = crate::expr::datums::binary_payload(&args[0], control)?;
    let input = if let Some(payload) = &binary {
        payload.bytes()
    } else {
        owned = value_to_string_with_control(&args[0], control)?;
        owned.as_bytes()
    };
    if name == "md5" {
        return string(md5_hex_with_control(input, control)?, control);
    }
    let encoding = value_to_string_with_control(&args[1], control)?;
    let output = match encoding.to_ascii_lowercase().as_str() {
        "hex" => {
            crate::expr::text_size::check_conversion_size(input.len() as u64 * 2, "encoding")?;
            hex_encode_with_control(input, control)?
        }
        "base64" => base64_encode_with_control(input, control)?,
        "escape" => escape_encode_with_control(input, control)?,
        _ => return Err(unrecognized_encoding(&encoding)),
    };
    string(output, control)
}

/// An encoding `encode` and `decode` do not know, whose name `PostgreSQL` compares without regard to case.
fn unrecognized_encoding(name: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "22023".into(),
        message: format!("unrecognized encoding: \"{name}\""),
    }
}

fn decode(args: &[Value], control: &ProductionControl<'_>) -> Result<Produced<Value>> {
    if args.len() != 2 {
        return Err(SQLError::TypeMismatch("decode takes 2 args".into()));
    }
    let s = crate::expr::conversion::argument_text(&args[0], control)?;
    let encoding = value_to_string_with_control(&args[1], control)?;
    let output = match encoding.to_ascii_lowercase().as_str() {
        "hex" => hex_decode_with_control(&s, control)?,
        "base64" => base64_decode_with_control(&s, control)?,
        "escape" => escape_decode_with_control(&s, control)?,
        _ => return Err(unrecognized_encoding(&encoding)),
    };
    bytes(output, control)
}

fn split_part(args: &[Value], control: &ProductionControl<'_>) -> Result<Produced<Value>> {
    if args.len() != 3 {
        return Err(SQLError::TypeMismatch("split_part takes 3 args".into()));
    }
    if args.iter().any(|arg| matches!(arg, Value::Null)) {
        return plain(Value::Null, control);
    }
    let s = value_to_string_with_control(&args[0], control)?;
    let separator = value_to_string_with_control(&args[1], control)?;
    let index = to_i64_with_control(&args[2], control)?;
    if index == 0 {
        return Err(SQLError::Routine {
            sqlstate: "22023".into(),
            message: "field position must not be zero".into(),
        });
    }
    let mut parts = ProductionVec::new(*control);
    if separator.is_empty() {
        parts.push_copy(s.as_str())?;
    } else {
        for part in s.split(separator.as_str()) {
            parts.push_copy(part)?;
        }
    }
    let index = if index >= 1 {
        usize::try_from(index - 1).ok()
    } else {
        usize::try_from(index.unsigned_abs())
            .ok()
            .and_then(|from_end| parts.len().checked_sub(from_end))
    };
    string(
        control.copy_text(
            index
                .and_then(|index| parts.get(index))
                .copied()
                .unwrap_or(""),
        )?,
        control,
    )
}
