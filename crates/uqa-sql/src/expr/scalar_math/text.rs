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
    conversion::{to_i64_with_control, value_to_string_with_control},
    encoding::{
        base64_decode_with_control, base64_encode_with_control, hex_encode_with_control,
        md5_hex_with_control,
    },
    json::utf8_lossy_with_control,
    nonnegative_usize,
};

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
            let n = nonnegative_usize(
                to_i64_with_control(&args[1], control)?.max(0),
                "lpad/rpad length",
            )?;
            let fill = match args.get(2) {
                Some(value) => value_to_string_with_control(value, control)?,
                None => control.copy_text(" ")?,
            };
            let chars = characters(&s, control)?;
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
            let n = nonnegative_usize(
                to_i64_with_control(&args[1], control)?.max(0),
                "repeat count",
            )?;
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
        "overlay" => {
            if args.len() < 3 || args.len() > 4 {
                return Err(SQLError::TypeMismatch("overlay takes 3 or 4 args".into()));
            }
            let s = characters(&value_to_string_with_control(&args[0], control)?, control)?;
            let placing = characters(&value_to_string_with_control(&args[1], control)?, control)?;
            let start = nonnegative_usize(
                to_i64_with_control(&args[2], control)?.max(1) - 1,
                "overlay start position",
            )?;
            let len = if args.len() == 4 {
                nonnegative_usize(
                    to_i64_with_control(&args[3], control)?.max(0),
                    "overlay length",
                )?
            } else {
                placing.len()
            };
            let end = start.saturating_add(len).min(s.len());
            let mut out = ProductionString::new(*control);
            for character in s[..start.min(s.len())]
                .iter()
                .chain(placing.iter())
                .chain(s[end..].iter())
            {
                out.push(*character)?;
            }
            string(out.finish()?, control)
        }
        "md5" | "encode" => encode(name, args, control),
        "decode" => decode(args, control),
        "split_part" => split_part(args, control),
        _ => unreachable!("text family membership was checked before dispatch"),
    }
}

pub(super) fn ordinary_format(args: &[Value]) -> Result<Value> {
    use crate::expr::{coerce_i64, value_to_string};
    fn format_argument_to_string(value: &Value) -> String {
        match value {
            Value::Bool(true) => "t".into(),
            Value::Bool(false) => "f".into(),
            other => value_to_string(other),
        }
    }
    if args.is_empty() {
        return Err(SQLError::TypeMismatch(
            "format needs a format string".into(),
        ));
    }
    let fmt = value_to_string(&args[0]);
    let mut out = String::with_capacity(fmt.len());
    let mut iter = fmt.chars().peekable();
    let mut idx = 1usize;
    while let Some(c) = iter.next() {
        if c == '%' {
            match iter.next() {
                Some('s') | Some('I') | Some('L') => {
                    out.push_str(&format_argument_to_string(
                        args.get(idx).unwrap_or(&Value::Null),
                    ));
                    idx += 1;
                }
                Some('d') => {
                    let n = args.get(idx).and_then(|v| coerce_i64(v)).unwrap_or(0);
                    out.push_str(&n.to_string());
                    idx += 1;
                }
                Some('%') => out.push('%'),
                Some(other) => out.push(other),
                None => out.push('%'),
            }
        } else {
            out.push(c);
        }
    }
    Ok(Value::Str(out))
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
    let input = match &args[0] {
        Value::Bytes(bytes) => bytes.as_slice(),
        value => {
            owned = value_to_string_with_control(value, control)?;
            owned.as_bytes()
        }
    };
    if name == "md5" {
        return string(md5_hex_with_control(input, control)?, control);
    }
    let encoding = value_to_string_with_control(&args[1], control)?;
    let output = match encoding.as_str() {
        "hex" => hex_encode_with_control(input, control)?,
        "base64" => base64_encode_with_control(input, control)?,
        "escape" => {
            let input = utf8_lossy_with_control(input, control)?;
            let mut out = ProductionString::new(*control);
            for character in input.escape_default() {
                out.push(character)?;
            }
            out.finish()?
        }
        other => {
            return Err(SQLError::TypeMismatch(format!(
                "unknown encoding {other:?}"
            )))
        }
    };
    string(output, control)
}

fn decode(args: &[Value], control: &ProductionControl<'_>) -> Result<Produced<Value>> {
    if args.len() != 2 {
        return Err(SQLError::TypeMismatch("decode takes 2 args".into()));
    }
    let s = value_to_string_with_control(&args[0], control)?;
    let encoding = value_to_string_with_control(&args[1], control)?;
    match encoding.as_str() {
        "hex" => {
            let mut cleaned = ProductionString::new(*control);
            for character in s.chars() {
                control.check()?;
                if !character.is_whitespace() {
                    cleaned.push(character)?;
                }
            }
            if !cleaned.len().is_multiple_of(2) {
                return Err(SQLError::TypeMismatch(
                    "invalid hexadecimal data: odd number of digits".into(),
                ));
            }
            let mut out = ProductionVec::new(*control);
            out.reserve(cleaned.len() / 2)?;
            for pair in cleaned.as_bytes().chunks_exact(2) {
                let hi = (pair[0] as char)
                    .to_digit(16)
                    .ok_or_else(|| SQLError::TypeMismatch("invalid hexadecimal digit".into()))?
                    as u8;
                let lo = (pair[1] as char)
                    .to_digit(16)
                    .ok_or_else(|| SQLError::TypeMismatch("invalid hexadecimal digit".into()))?
                    as u8;
                out.push_copy(hi * 16 + lo)?;
            }
            bytes(out.finish()?, control)
        }
        "base64" => {
            let output = base64_decode_with_control(&s, control).map_err(|error| match error {
                SQLError::TypeMismatch(_) => {
                    SQLError::TypeMismatch(format!("base64 decode: {error}"))
                }
                error => error,
            })?;
            bytes(output, control)
        }
        "escape" => {
            let (value, memory) = s.into_parts();
            Ok(control.finish(Value::Bytes(value.into_bytes()), memory)?)
        }
        other => Err(SQLError::TypeMismatch(format!(
            "unknown encoding {other:?}"
        ))),
    }
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
