//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Rendering for parsed `PostgreSQL` numeric format pictures.

use uqa_core::{DecimalValue, Value};

use crate::error::{Result, SQLError};

use super::picture::{is_numeric_picture_marker, NumericPicture};
use super::sign::{
    apply_float_aware_numeric_sign, apply_truncated_special_sign, FloatFractionTruncation,
};

#[expect(
    clippy::too_many_lines,
    reason = "ordered PostgreSQL format state machine preserves token precedence"
)]
pub(super) fn format_pg_number_picture(value: &Value, picture: NumericPicture) -> Result<String> {
    let NumericPicture {
        fill_mode,
        template,
        sign_format,
    } = picture;
    let template = template.as_str();
    let (integer_template, fractional_template, decimal_token_present) = template
        .split_once('.')
        .map_or((template, None, false), |(integer, fractional)| {
            (integer, Some(fractional), true)
        });
    let fractional_template = fractional_template.filter(|template| {
        template
            .chars()
            .any(|placeholder| matches!(placeholder, '9' | '0'))
    });
    let integer_digit_template = integer_template
        .chars()
        .filter(|placeholder| matches!(placeholder, '9' | '0'))
        .collect::<String>();
    let fractional_digit_template = fractional_template
        .unwrap_or_default()
        .chars()
        .filter(|placeholder| matches!(placeholder, '9' | '0'))
        .collect::<String>();
    let integer_width = integer_digit_template.len();
    let requested_fractional_digits = fractional_digit_template.len();
    let (negative, rendered, fractional_digits, float_fraction_truncation) = match value {
        Value::Int(value) => {
            let value = DecimalValue::from_i64(*value);
            let (negative, rendered) = rounded_decimal_text(&value, requested_fractional_digits)?;
            (
                negative,
                rendered,
                requested_fractional_digits,
                FloatFractionTruncation::None,
            )
        }
        Value::Decimal(value) => {
            let (negative, rendered) = rounded_decimal_text(value, requested_fractional_digits)?;
            (
                negative,
                rendered,
                requested_fractional_digits,
                FloatFractionTruncation::None,
            )
        }
        Value::Float(value) if value.is_nan() => (
            false,
            "NaN".into(),
            requested_fractional_digits,
            FloatFractionTruncation::None,
        ),
        Value::Float(value) if value.is_infinite() => (
            value.is_sign_negative(),
            "Infinity".into(),
            requested_fractional_digits,
            FloatFractionTruncation::None,
        ),
        Value::Float(value) => {
            let integer_digits = format!("{:.0}", value.abs()).len();
            let available_fractional_digits = (f64::DIGITS as usize).saturating_sub(integer_digits);
            let fractional_digits = requested_fractional_digits.min(available_fractional_digits);
            let truncation = if fractional_digits == requested_fractional_digits {
                FloatFractionTruncation::None
            } else if fractional_template.is_some_and(|_| {
                fractional_digit_template[fractional_digits..]
                    .chars()
                    .all(|placeholder| placeholder == '9')
            }) {
                FloatFractionTruncation::NinePlaceholder
            } else {
                FloatFractionTruncation::ZeroPlaceholder
            };
            (
                value.is_sign_negative(),
                format!("{:.fractional_digits$}", value.abs()),
                fractional_digits,
                truncation,
            )
        }
        _ => {
            return Err(SQLError::TypeMismatch(format!(
                "to_char: unsupported numeric source {value:?}"
            )))
        }
    };
    if matches!(rendered.as_str(), "NaN" | "Infinity") {
        if rendered.len() > integer_width {
            let integer = render_integer_numeric_picture(
                integer_template,
                &"#".repeat(integer_width),
                fill_mode,
            );
            let fractional = if decimal_token_present {
                format!(
                    ".{}",
                    render_fractional_numeric_picture(
                        fractional_template.unwrap_or_default(),
                        &"#".repeat(fractional_digits),
                        fill_mode,
                        float_fraction_truncation.stops_picture_scan(fill_mode),
                    )
                )
            } else {
                String::new()
            };
            return Ok(apply_float_aware_numeric_sign(
                format!("{integer}{fractional}"),
                negative,
                fill_mode,
                sign_format,
                float_fraction_truncation,
            ));
        }
        let slots =
            numeric_picture_slots(integer_width, &rendered, integer_digit_template.find('0'));
        let body = render_integer_numeric_picture(integer_template, &slots, fill_mode);
        if decimal_token_present {
            return Ok(apply_truncated_special_sign(
                body,
                negative,
                fill_mode,
                sign_format,
            ));
        }
        return Ok(super::sign::apply_numeric_sign(
            body,
            negative,
            fill_mode,
            sign_format,
        ));
    }
    let (integer_digits, mut fractional) = rendered.split_once('.').map_or_else(
        || (rendered.as_str(), String::new()),
        |(integer, fractional)| (integer, fractional.to_string()),
    );
    fractional.extend(std::iter::repeat_n(
        '0',
        fractional_digits.saturating_sub(fractional.len()),
    ));
    let rounded_zero = integer_digits == "0" && fractional.bytes().all(|digit| digit == b'0');
    let integer_digits = if integer_digits == "0"
        && fractional_template.is_some()
        && !integer_digit_template.is_empty()
        && integer_digit_template
            .chars()
            .all(|placeholder| placeholder == '9')
        && !(fill_mode
            && rounded_zero
            && fractional_template.is_some_and(|_| {
                fractional_digit_template
                    .chars()
                    .all(|placeholder| placeholder == '9')
            })) {
        ""
    } else {
        integer_digits
    };
    if integer_digits.len() > integer_width {
        let integer =
            render_integer_numeric_picture(integer_template, &"#".repeat(integer_width), fill_mode);
        let fractional = if decimal_token_present {
            format!(
                ".{}",
                render_fractional_numeric_picture(
                    fractional_template.unwrap_or_default(),
                    &"#".repeat(fractional_digits),
                    fill_mode,
                    float_fraction_truncation.stops_picture_scan(fill_mode),
                )
            )
        } else {
            String::new()
        };
        return Ok(apply_float_aware_numeric_sign(
            format!("{integer}{fractional}"),
            negative,
            fill_mode,
            sign_format,
            float_fraction_truncation,
        ));
    }
    let slots = numeric_picture_slots(
        integer_width,
        integer_digits,
        integer_digit_template.find('0'),
    );
    let integer = render_integer_numeric_picture(integer_template, &slots, fill_mode);
    if fill_mode {
        if fractional_template.is_some() {
            while fractional.ends_with('0') {
                let position = fractional.len() - 1;
                if fractional_digit_template.as_bytes()[position] != b'9'
                    || fractional_digit_template[position..]
                        .bytes()
                        .any(|placeholder| placeholder == b'0')
                {
                    break;
                }
                fractional.pop();
            }
        }
        let fractional = render_fractional_numeric_picture(
            fractional_template.unwrap_or_default(),
            &fractional,
            true,
            float_fraction_truncation.stops_picture_scan(true),
        );
        let body = fractional_template
            .map_or_else(|| integer.clone(), |_| format!("{integer}.{fractional}"));
        if decimal_token_present && fractional_template.is_none() {
            return Ok(apply_truncated_special_sign(
                body,
                negative,
                true,
                sign_format,
            ));
        }
        return Ok(apply_float_aware_numeric_sign(
            body,
            negative,
            true,
            sign_format,
            float_fraction_truncation,
        ));
    }
    let fractional = render_fractional_numeric_picture(
        fractional_template.unwrap_or_default(),
        &fractional,
        false,
        float_fraction_truncation.stops_picture_scan(false),
    );
    let body =
        fractional_template.map_or_else(|| integer.clone(), |_| format!("{integer}.{fractional}"));
    if decimal_token_present && fractional_template.is_none() {
        return Ok(apply_truncated_special_sign(
            body,
            negative,
            false,
            sign_format,
        ));
    }
    Ok(apply_float_aware_numeric_sign(
        body,
        negative,
        false,
        sign_format,
        float_fraction_truncation,
    ))
}

fn numeric_picture_slots(width: usize, content: &str, first_zero: Option<usize>) -> String {
    let padding = width.saturating_sub(content.len());
    let mut slots = (0..padding)
        .map(|index| {
            if first_zero.is_some_and(|first_zero| index >= first_zero) {
                '0'
            } else {
                ' '
            }
        })
        .collect::<String>();
    slots.push_str(content);
    slots
}

fn render_integer_numeric_picture(template: &str, slots: &str, fill_mode: bool) -> String {
    let mut slots = slots.chars();
    let mut output = String::with_capacity(template.len());
    let mut number_started = false;
    for token in template.chars() {
        match token {
            '9' | '0' => {
                let slot = slots.next().expect("numeric picture slot count");
                if slot != ' ' {
                    number_started = true;
                    output.push(slot);
                } else if !fill_mode {
                    output.push(' ');
                }
            }
            ',' if number_started => output.push(','),
            ',' if !fill_mode => output.push(' '),
            ',' => {}
            marker if is_numeric_picture_marker(marker) => output.push(marker),
            _ => unreachable!("validated integer numeric picture token"),
        }
    }
    output
}

fn render_fractional_numeric_picture(
    template: &str,
    slots: &str,
    fill_mode: bool,
    truncated_number_buffer: bool,
) -> String {
    let mut slots = slots.chars();
    let mut output = String::with_capacity(template.len());
    let mut number_in = false;
    for token in template.chars() {
        match token {
            '9' | '0' => {
                if let Some(slot) = slots.next() {
                    output.push(slot);
                    number_in = true;
                } else if truncated_number_buffer {
                    break;
                } else {
                    number_in = false;
                }
            }
            ',' if number_in => output.push(','),
            ',' if !fill_mode => output.push(' '),
            ',' => {}
            marker if is_numeric_picture_marker(marker) => output.push(marker),
            _ => unreachable!("validated fractional numeric picture token"),
        }
    }
    output
}

fn rounded_decimal_text(value: &DecimalValue, fractional_digits: usize) -> Result<(bool, String)> {
    let scale = i32::try_from(fractional_digits)
        .map_err(|_| SQLError::TypeMismatch("to_char: numeric format scale out of range".into()))?;
    let rounded = value
        .abs()
        .round_to_scale(scale)
        .ok_or_else(|| SQLError::TypeMismatch("to_char: numeric out of range".into()))?;
    let negative = value.is_negative() && !rounded.is_zero();
    Ok((negative, rounded.to_sql_string()))
}

pub(super) fn numeric_text(value: &Value) -> String {
    match value {
        Value::Int(value) => value.to_string(),
        Value::Float(value) if value.is_nan() => "NaN".into(),
        Value::Float(value) if *value == f64::INFINITY => "Infinity".into(),
        Value::Float(value) if *value == f64::NEG_INFINITY => "-Infinity".into(),
        Value::Float(value) => value.to_string(),
        Value::Decimal(value) => value.to_sql_string(),
        _ => unreachable!("numeric formatter input"),
    }
}
