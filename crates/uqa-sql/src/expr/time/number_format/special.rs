//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scientific and Roman numeric output formats.

use uqa_core::{DecimalValue, Value};

use crate::error::{Result, SQLError};

pub(super) fn format_pg_scientific_number(value: &Value, fmt: &str) -> Result<Option<String>> {
    let Some(picture) = fmt
        .strip_suffix("EEEE")
        .or_else(|| fmt.strip_suffix("eeee"))
    else {
        return Ok(None);
    };
    let picture = picture
        .chars()
        .map(|character| {
            if matches!(character, 'D' | 'd') {
                '.'
            } else {
                character
            }
        })
        .collect::<String>();
    if picture.is_empty()
        || !picture
            .chars()
            .all(|character| matches!(character, '9' | '0' | '.'))
        || picture.matches('.').count() > 1
    {
        return Err(SQLError::Routine {
            sqlstate: "42601".into(),
            message: "EEEE is incompatible with other formats".into(),
        });
    }
    let fractional_digits = picture
        .split_once('.')
        .map_or(0, |(_, fractional)| fractional.len());
    let (negative, special) = match value {
        Value::Float(value) if !value.is_finite() => (false, Some(value.to_string())),
        Value::Decimal(value) if value.is_nan() || value.is_infinite() => {
            (false, Some(value.to_sql_string()))
        }
        _ => (false, None),
    };
    if special.is_some() {
        let mut hashes = picture
            .chars()
            .map(|character| {
                if matches!(character, '9' | '0') {
                    '#'
                } else {
                    character
                }
            })
            .collect::<String>();
        if !hashes.contains('.') {
            hashes.push('.');
        }
        hashes.push_str("####");
        return Ok(Some(format!(
            "{}{hashes}",
            if negative { '-' } else { ' ' }
        )));
    }
    let (negative, mantissa, exponent) = match value {
        Value::Int(value) => {
            scientific_decimal_parts(&DecimalValue::from_i64(*value), fractional_digits)?
        }
        Value::Decimal(value) => scientific_decimal_parts(value, fractional_digits)?,
        Value::Float(value) => scientific_float_parts(*value, fractional_digits),
        _ => {
            return Err(SQLError::TypeMismatch(format!(
                "to_char: unsupported numeric source {value:?}"
            )))
        }
    };
    let exponent_sign = if exponent < 0 { '-' } else { '+' };
    let exponent = exponent.unsigned_abs();
    Ok(Some(format!(
        "{}{mantissa}e{exponent_sign}{exponent:02}",
        if negative { '-' } else { ' ' }
    )))
}

fn scientific_decimal_parts(
    value: &DecimalValue,
    fractional_digits: usize,
) -> Result<(bool, String, i64)> {
    let negative = value.is_negative();
    let value = value.abs();
    if value.is_zero() {
        return Ok((
            false,
            fixed_fractional_text("0".into(), fractional_digits),
            0,
        ));
    }
    let (coefficient, scale) = value.canonical_parts();
    let coefficient_digits = i64::try_from(coefficient.len())
        .map_err(|_| SQLError::TypeMismatch("to_char: numeric out of range".into()))?;
    let exponent = coefficient_digits - i64::from(scale) - 1;
    let mantissa_exponent = 1 - coefficient_digits;
    let mantissa = DecimalValue::parse(&format!("{coefficient}e{mantissa_exponent}"))
        .and_then(|value| {
            i32::try_from(fractional_digits)
                .ok()
                .and_then(|scale| value.round_to_scale(scale))
        })
        .ok_or_else(|| SQLError::TypeMismatch("to_char: numeric out of range".into()))?;
    Ok((
        negative,
        fixed_fractional_text(mantissa.to_sql_string(), fractional_digits),
        exponent,
    ))
}

fn scientific_float_parts(value: f64, fractional_digits: usize) -> (bool, String, i64) {
    let negative = value.is_sign_negative();
    let value = value.abs();
    if value == 0.0 {
        return (
            negative,
            fixed_fractional_text("0".into(), fractional_digits),
            0,
        );
    }
    let mut exponent = value.log10().floor() as i64;
    let mantissa = value / 10_f64.powi(i32::try_from(exponent).unwrap_or(0));
    let mut mantissa = format!("{mantissa:.fractional_digits$}");
    if mantissa
        .parse::<f64>()
        .is_ok_and(|mantissa| mantissa >= 10.0)
    {
        exponent += 1;
        mantissa = fixed_fractional_text("1".into(), fractional_digits);
    }
    (negative, mantissa, exponent)
}

fn fixed_fractional_text(mut text: String, fractional_digits: usize) -> String {
    if fractional_digits == 0 {
        return text
            .split_once('.')
            .map_or(text.clone(), |(integer, _)| integer.into());
    }
    if !text.contains('.') {
        text.push('.');
    }
    let existing = text
        .split_once('.')
        .map_or(0, |(_, fractional)| fractional.len());
    text.extend(std::iter::repeat_n(
        '0',
        fractional_digits.saturating_sub(existing),
    ));
    text
}

pub(super) fn format_pg_roman_number(value: &Value, fmt: &str) -> Result<Option<String>> {
    let (fill_mode, lowercase) = match fmt {
        "RN" => (false, false),
        "rn" => (false, true),
        "FMRN" | "fmRN" => (true, false),
        "FMrn" | "fmrn" => (true, true),
        _ => return Ok(None),
    };
    if matches!(value, Value::Null) {
        return Ok(Some(String::new()));
    }
    let rounded = match value {
        Value::Int(value) => Some(*value),
        Value::Decimal(value) => value
            .round_to_scale(0)
            .and_then(|value| value.to_i64_trunc()),
        Value::Float(value) if value.is_finite() => format!("{value:.0}").parse().ok(),
        _ => None,
    };
    let Some(value) = rounded.filter(|value| (1..=3999).contains(value)) else {
        return Ok(Some("#".repeat(15)));
    };
    let mut value = u16::try_from(value).expect("validated Roman numeral range");
    let mut roman = String::new();
    for (number, digits) in [
        (1000, "M"),
        (900, "CM"),
        (500, "D"),
        (400, "CD"),
        (100, "C"),
        (90, "XC"),
        (50, "L"),
        (40, "XL"),
        (10, "X"),
        (9, "IX"),
        (5, "V"),
        (4, "IV"),
        (1, "I"),
    ] {
        while value >= number {
            roman.push_str(digits);
            value -= number;
        }
    }
    if lowercase {
        roman.make_ascii_lowercase();
    }
    if !fill_mode {
        roman.insert_str(0, &" ".repeat(15 - roman.len()));
    }
    Ok(Some(roman))
}
