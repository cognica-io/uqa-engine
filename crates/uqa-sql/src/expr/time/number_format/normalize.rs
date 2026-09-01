//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Normalization for `PostgreSQL` numeric format tokens.

use uqa_core::{DecimalValue, Value};

use crate::error::{Result, SQLError};

use super::super::out_of_range;

pub(super) struct NormalizedNumericFormat {
    pub(super) picture: String,
    pub(super) literal_output: String,
    pub(super) scale: usize,
    pub(super) ordinal: Option<bool>,
    pub(super) has_digit: bool,
    pub(super) has_decimal: bool,
    pub(super) recognized_token: bool,
}

#[expect(
    clippy::too_many_lines,
    reason = "ordered PostgreSQL format state machine preserves token precedence"
)]
pub(super) fn normalize_numeric_format(fmt: &str) -> Result<NormalizedNumericFormat> {
    let mut picture = String::with_capacity(fmt.len());
    let mut literal_output = String::new();
    let mut scale = 0usize;
    let mut ordinal = None;
    let mut has_digit = false;
    let mut has_decimal = false;
    let mut recognized_token = false;
    let mut after_scale = false;
    let mut remaining = fmt;
    while !remaining.is_empty() {
        if remaining.starts_with('"') {
            let (quoted, rest, literal) = take_numeric_quoted_literal(remaining);
            picture.push_str(quoted);
            literal_output.push_str(&literal);
            remaining = rest;
            continue;
        }
        if let Some(rest) = remaining.strip_prefix("TH") {
            ordinal = Some(true);
            recognized_token = true;
            remaining = rest;
            continue;
        }
        if let Some(rest) = remaining.strip_prefix("th") {
            ordinal = Some(false);
            recognized_token = true;
            remaining = rest;
            continue;
        }
        if let Some(rest) = remaining
            .strip_prefix("SP")
            .or_else(|| remaining.strip_prefix("sp"))
        {
            recognized_token = true;
            remaining = rest;
            continue;
        }
        if let Some(rest) = remaining
            .strip_prefix("FM")
            .or_else(|| remaining.strip_prefix("fm"))
        {
            picture.push_str(&remaining[..2]);
            recognized_token = true;
            remaining = rest;
            continue;
        }
        let mut matched_sign = false;
        for token in ["MI", "mi", "PL", "pl", "SG", "sg", "PR", "pr"] {
            if let Some(rest) = remaining.strip_prefix(token) {
                picture.push_str(token);
                recognized_token = true;
                remaining = rest;
                matched_sign = true;
                break;
            }
        }
        if matched_sign {
            continue;
        }
        let character = remaining
            .chars()
            .next()
            .expect("non-empty numeric format remainder");
        let rest = &remaining[character.len_utf8()..];
        match character {
            '9' | '0' => {
                picture.push(character);
                has_digit = true;
                recognized_token = true;
                if after_scale {
                    scale = scale.checked_add(1).ok_or_else(|| {
                        SQLError::TypeMismatch("to_char: numeric format scale out of range".into())
                    })?;
                }
            }
            '.' => {
                if after_scale {
                    return Err(SQLError::Routine {
                        sqlstate: "42601".into(),
                        message: "cannot use V and decimal point together".into(),
                    });
                }
                picture.push('.');
                has_decimal = true;
                recognized_token = true;
            }
            'D' | 'd' => {
                if after_scale {
                    return Err(SQLError::Routine {
                        sqlstate: "42601".into(),
                        message: "cannot use V and decimal point together".into(),
                    });
                }
                picture.push('.');
                has_decimal = true;
                recognized_token = true;
            }
            ',' | 'G' | 'g' => {
                picture.push(',');
                recognized_token = true;
            }
            'L' | 'l' => {
                picture.push_str(r#""$""#);
                literal_output.push('$');
                recognized_token = true;
            }
            'B' | 'b' | 'C' | 'c' => {
                recognized_token = true;
            }
            'V' | 'v' => {
                if has_decimal {
                    return Err(SQLError::Routine {
                        sqlstate: "42601".into(),
                        message: "cannot use V and decimal point together".into(),
                    });
                }
                after_scale = true;
                recognized_token = true;
            }
            'S' | 's' => {
                picture.push(character);
                recognized_token = true;
            }
            '\\' if rest.starts_with('"') => {
                picture.push('\\');
                picture.push('"');
                literal_output.push('"');
                remaining = &rest['"'.len_utf8()..];
                continue;
            }
            _ => {
                picture.push(character);
                literal_output.push(character);
            }
        }
        remaining = rest;
    }
    Ok(NormalizedNumericFormat {
        picture,
        literal_output,
        scale,
        ordinal,
        has_digit,
        has_decimal,
        recognized_token,
    })
}

fn take_numeric_quoted_literal(input: &str) -> (&str, &str, String) {
    let mut escaped = false;
    let mut literal = String::new();
    for (position, character) in input['"'.len_utf8()..].char_indices() {
        if escaped {
            literal.push(character);
            escaped = false;
            continue;
        }
        if character == '\\' {
            escaped = true;
            continue;
        }
        if character == '"' {
            let end = '"'.len_utf8() + position + character.len_utf8();
            return (&input[..end], &input[end..], literal);
        }
        literal.push(character);
    }
    (input, "", literal)
}

pub(super) fn scale_numeric_format_value(value: &Value, scale: usize) -> Result<Value> {
    match value {
        Value::Float(value) => {
            let scale = i32::try_from(scale).map_err(|_| {
                SQLError::TypeMismatch("to_char: numeric format scale out of range".into())
            })?;
            Ok(Value::Float(*value * 10_f64.powi(scale)))
        }
        Value::Int(value) => scale_decimal_format_value(&DecimalValue::from_i64(*value), scale),
        Value::Decimal(value) => scale_decimal_format_value(value, scale),
        _ => Err(SQLError::TypeMismatch(format!(
            "to_char: unsupported numeric source {value:?}"
        ))),
    }
}

fn scale_decimal_format_value(value: &DecimalValue, scale: usize) -> Result<Value> {
    let factor = DecimalValue::parse(&format!("1e{scale}")).ok_or_else(|| {
        SQLError::TypeMismatch("to_char: numeric format scale out of range".into())
    })?;
    value
        .checked_mul(&factor)
        .map(Value::Decimal)
        .ok_or_else(|| out_of_range("numeric"))
}

pub(super) fn ordinal_suffix(digits: &str, upper: bool) -> String {
    let last_two = digits
        .bytes()
        .rev()
        .take(2)
        .enumerate()
        .fold(0_u8, |value, (position, digit)| {
            value + (digit - b'0') * if position == 0 { 1 } else { 10 }
        });
    let suffix = if (11..=13).contains(&last_two) {
        "th"
    } else {
        match last_two % 10 {
            1 => "st",
            2 => "nd",
            3 => "rd",
            _ => "th",
        }
    };
    if upper {
        suffix.to_ascii_uppercase()
    } else {
        suffix.into()
    }
}
