//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18 numeric formatting for `to_char`.

use uqa_core::Value;

use crate::error::Result;

use normalize::{normalize_numeric_format, ordinal_suffix, scale_numeric_format_value};
use picture::{decode_numeric_format_literal, parse_numeric_picture, split_outer_numeric_literals};
use render::{format_pg_number_picture, numeric_text};
use special::{format_pg_roman_number, format_pg_scientific_number};

mod normalize;
mod picture;
mod render;
mod sign;
mod special;

pub(super) fn format_pg_number(value: &Value, fmt: &str) -> Result<String> {
    if let Some(formatted) = format_pg_scientific_number(value, fmt)? {
        return Ok(formatted);
    }
    if let Some(formatted) = format_pg_roman_number(value, fmt)? {
        return Ok(formatted);
    }
    let normalized = normalize_numeric_format(fmt)?;
    if !normalized.has_digit {
        return Ok(if normalized.recognized_token {
            normalized.literal_output
        } else {
            decode_numeric_format_literal(fmt)
        });
    }
    let scaled;
    let value = if normalized.scale == 0 {
        value
    } else {
        scaled = scale_numeric_format_value(value, normalized.scale)?;
        &scaled
    };
    let mut formatted = format_pg_number_basic(value, &normalized.picture)?;
    if let Some(upper) = normalized.ordinal {
        if !normalized.has_decimal && !formatted.contains('-') && !formatted.contains('#') {
            let digits = formatted
                .chars()
                .filter(char::is_ascii_digit)
                .collect::<String>();
            if !digits.is_empty() {
                formatted.push_str(&ordinal_suffix(&digits, upper));
            }
        }
    }
    Ok(formatted)
}

fn format_pg_number_basic(value: &Value, fmt: &str) -> Result<String> {
    if let Some(picture) = parse_numeric_picture(fmt)? {
        return format_pg_number_picture(value, picture);
    }
    if let Some((prefix, picture, suffix)) = split_outer_numeric_literals(fmt) {
        if let Some(picture) = parse_numeric_picture(picture)? {
            return Ok(format!(
                "{}{}{}",
                decode_numeric_format_literal(prefix),
                format_pg_number_picture(value, picture)?,
                decode_numeric_format_literal(suffix)
            ));
        }
    }
    Ok(numeric_text(value))
}

#[cfg(test)]
mod tests;
