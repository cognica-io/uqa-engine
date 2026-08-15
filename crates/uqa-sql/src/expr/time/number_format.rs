//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18 numeric formatting for `to_char`.

use uqa_core::{DecimalValue, Value};

use crate::error::{Result, SQLError};

use super::out_of_range;
use sign::{apply_explicit_numeric_signs, apply_numeric_brackets, apply_positioned_numeric_sign};

mod sign;

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

struct NormalizedNumericFormat {
    picture: String,
    literal_output: String,
    scale: usize,
    ordinal: Option<bool>,
    has_digit: bool,
    has_decimal: bool,
    recognized_token: bool,
}

fn normalize_numeric_format(fmt: &str) -> Result<NormalizedNumericFormat> {
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

fn scale_numeric_format_value(value: &Value, scale: usize) -> Result<Value> {
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

fn ordinal_suffix(digits: &str, upper: bool) -> String {
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

fn format_pg_scientific_number(value: &Value, fmt: &str) -> Result<Option<String>> {
    let Some(picture) = fmt
        .strip_suffix("EEEE")
        .or_else(|| fmt.strip_suffix("eeee"))
    else {
        return Ok(None);
    };
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

fn format_pg_roman_number(value: &Value, fmt: &str) -> Result<Option<String>> {
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

const NUMERIC_MINUS_MARKER: char = '\u{1}';
const NUMERIC_PLUS_MARKER: char = '\u{2}';
const NUMERIC_SIGN_MARKER: char = '\u{3}';
const NUMERIC_BRACKET_MARKER: char = '\u{4}';

struct NumericPicture {
    fill_mode: bool,
    template: String,
    sign_format: NumericSignFormat,
}

fn parse_numeric_picture(fmt: &str) -> Result<Option<NumericPicture>> {
    let mut fill_mode = false;
    let mut template = String::with_capacity(fmt.len());
    let mut explicit_count = 0usize;
    let mut explicit_plus = false;
    let mut explicit_minus = false;
    let mut last_explicit = None;
    let mut locale_sign_position = None;
    let mut brackets = false;
    let mut remaining = fmt;
    while !remaining.is_empty() {
        if let Some(rest) = remaining
            .strip_prefix("FM")
            .or_else(|| remaining.strip_prefix("fm"))
        {
            fill_mode = true;
            remaining = rest;
            continue;
        }
        let mut matched_explicit = false;
        for (upper, lower, explicit) in [
            ("MI", "mi", ExplicitNumericSign::Minus),
            ("PL", "pl", ExplicitNumericSign::Plus),
            ("SG", "sg", ExplicitNumericSign::Sign),
        ] {
            if let Some(rest) = remaining
                .strip_prefix(upper)
                .or_else(|| remaining.strip_prefix(lower))
            {
                if locale_sign_position.is_some() {
                    return Err(numeric_format_syntax(format!(
                        "cannot use \"S\" and \"{upper}\" together"
                    )));
                }
                if brackets {
                    return Err(numeric_format_syntax(
                        "cannot use \"PR\" and \"S\"/\"PL\"/\"MI\"/\"SG\" together",
                    ));
                }
                template.push(explicit.marker());
                explicit_count += 1;
                explicit_plus |= matches!(
                    explicit,
                    ExplicitNumericSign::Plus | ExplicitNumericSign::Sign
                );
                explicit_minus |= matches!(
                    explicit,
                    ExplicitNumericSign::Minus | ExplicitNumericSign::Sign
                );
                last_explicit = Some(explicit);
                remaining = rest;
                matched_explicit = true;
                break;
            }
        }
        if matched_explicit {
            continue;
        }
        if let Some(rest) = remaining
            .strip_prefix("PR")
            .or_else(|| remaining.strip_prefix("pr"))
        {
            if locale_sign_position.is_some() || explicit_count != 0 {
                return Err(numeric_format_syntax(
                    "cannot use \"PR\" and \"S\"/\"PL\"/\"MI\"/\"SG\" together",
                ));
            }
            brackets = true;
            remaining = rest;
            continue;
        }
        if let Some(rest) = remaining
            .strip_prefix('S')
            .or_else(|| remaining.strip_prefix('s'))
        {
            if locale_sign_position.is_some() {
                return Err(numeric_format_syntax("cannot use \"S\" twice"));
            }
            if explicit_count != 0 || brackets {
                return Err(numeric_format_syntax(
                    "cannot use \"S\" and \"PL\"/\"MI\"/\"SG\"/\"PR\" together",
                ));
            }
            locale_sign_position = Some(template.len());
            remaining = rest;
            continue;
        }
        let Some(character) = remaining.chars().next() else {
            return Ok(None);
        };
        if !matches!(character, '9' | '0' | '.' | ',') {
            return Ok(None);
        }
        if brackets && matches!(character, '9' | '0') {
            return Err(numeric_format_syntax(format!(
                "\"{character}\" must be ahead of \"PR\""
            )));
        }
        if character == '.' && template.contains('.') {
            return Err(numeric_format_syntax("multiple decimal points"));
        }
        template.push(character);
        remaining = &remaining[character.len_utf8()..];
    }
    if !template
        .chars()
        .any(|character| matches!(character, '9' | '0'))
        || template.matches('.').count() > 1
    {
        return Ok(None);
    }
    let sign_format = if brackets {
        insert_after_last_numeric_token(&mut template, NUMERIC_BRACKET_MARKER);
        NumericSignFormat::Brackets
    } else if let Some(position) = locale_sign_position {
        let pre_end = template.find('.').unwrap_or(template.len());
        if position < pre_end
            && template[position..pre_end]
                .chars()
                .any(|token| matches!(token, '9' | '0'))
        {
            NumericSignFormat::Leading
        } else {
            insert_after_last_numeric_token(&mut template, NUMERIC_SIGN_MARKER);
            NumericSignFormat::Trailing
        }
    } else if explicit_count == 1
        && last_explicit.is_some_and(|explicit| template.ends_with(explicit.marker()))
    {
        let explicit = last_explicit.expect("one explicit numeric sign exists");
        template.pop();
        explicit.trailing_format()
    } else if explicit_count != 0 {
        NumericSignFormat::Explicit {
            implicit_sign: explicit_plus && !explicit_minus,
        }
    } else {
        NumericSignFormat::Default
    };
    Ok(Some(NumericPicture {
        fill_mode,
        template,
        sign_format,
    }))
}

fn insert_after_last_numeric_token(template: &mut String, marker: char) {
    let position = template
        .char_indices()
        .rfind(|(_, token)| matches!(token, '9' | '0' | '.'))
        .map_or(template.len(), |(position, token)| {
            position + token.len_utf8()
        });
    template.insert(position, marker);
}

fn numeric_format_syntax(message: impl Into<String>) -> SQLError {
    SQLError::Routine {
        sqlstate: "42601".into(),
        message: message.into(),
    }
}

#[derive(Clone, Copy)]
enum ExplicitNumericSign {
    Minus,
    Plus,
    Sign,
}

impl ExplicitNumericSign {
    fn marker(self) -> char {
        match self {
            Self::Minus => NUMERIC_MINUS_MARKER,
            Self::Plus => NUMERIC_PLUS_MARKER,
            Self::Sign => NUMERIC_SIGN_MARKER,
        }
    }

    fn trailing_format(self) -> NumericSignFormat {
        match self {
            Self::Minus => NumericSignFormat::Minus,
            Self::Plus => NumericSignFormat::Plus,
            Self::Sign => NumericSignFormat::Sign,
        }
    }
}

fn split_outer_numeric_literals(fmt: &str) -> Option<(&str, &str, &str)> {
    let mut quoted = false;
    let mut escaped = false;
    let mut first = None;
    let mut end = 0usize;
    for (position, character) in fmt.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if character == '\\' {
            escaped = true;
            continue;
        }
        if character == '"' {
            quoted = !quoted;
            continue;
        }
        if !quoted && matches!(character, '9' | '0' | '.' | ',') {
            first.get_or_insert(position);
            end = position + character.len_utf8();
        }
    }
    let mut start = first?;
    loop {
        let prefix = &fmt[..start];
        if prefix.ends_with("FM") || prefix.ends_with("fm") {
            start -= 2;
        } else if prefix.ends_with('S') || prefix.ends_with('s') {
            start -= 1;
        } else {
            break;
        }
    }
    loop {
        let mut matched = false;
        for token in [
            "FM", "fm", "MI", "mi", "PL", "pl", "SG", "sg", "PR", "pr", "S", "s",
        ] {
            if fmt[end..].starts_with(token) {
                end += token.len();
                matched = true;
                break;
            }
        }
        if !matched {
            break;
        }
    }
    Some((&fmt[..start], &fmt[start..end], &fmt[end..]))
}

fn decode_numeric_format_literal(fragment: &str) -> String {
    let mut output = String::with_capacity(fragment.len());
    let mut characters = fragment.chars().peekable();
    let mut quoted = false;
    while let Some(character) = characters.next() {
        if character == '"' {
            quoted = !quoted;
            continue;
        }
        if character == '\\' {
            if quoted {
                if let Some(character) = characters.next() {
                    output.push(character);
                }
                continue;
            }
            if characters.peek() == Some(&'"') {
                output.push(characters.next().expect("peeked quote must exist"));
                continue;
            }
        }
        output.push(character);
    }
    output
}

fn format_pg_number_picture(value: &Value, picture: NumericPicture) -> Result<String> {
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
        return Ok(apply_numeric_sign(body, negative, fill_mode, sign_format));
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

fn apply_truncated_special_sign(
    body: String,
    negative: bool,
    fill_mode: bool,
    sign_format: NumericSignFormat,
) -> String {
    match sign_format {
        NumericSignFormat::Default => default_numeric_sign(body, negative, fill_mode),
        NumericSignFormat::Leading => anchored_numeric_sign(body, negative),
        NumericSignFormat::Explicit { implicit_sign } => {
            apply_explicit_numeric_signs(body, negative, fill_mode, implicit_sign)
        }
        NumericSignFormat::Trailing | NumericSignFormat::Minus | NumericSignFormat::Sign => body,
        NumericSignFormat::Plus if fill_mode => {
            if negative {
                format!("-{body}")
            } else {
                body
            }
        }
        NumericSignFormat::Plus => default_numeric_sign(body, negative, false),
        NumericSignFormat::Brackets if negative => {
            let first_digit = body.find(|character: char| character != ' ').unwrap_or(0);
            let (padding, number) = body.split_at(first_digit);
            format!("{padding}<{number}")
        }
        NumericSignFormat::Brackets => default_numeric_sign(body, false, fill_mode),
    }
}

fn apply_float_aware_numeric_sign(
    body: String,
    negative: bool,
    fill_mode: bool,
    sign_format: NumericSignFormat,
    truncation: FloatFractionTruncation,
) -> String {
    if truncation == FloatFractionTruncation::None {
        return apply_numeric_sign(body, negative, fill_mode, sign_format);
    }
    let truncated_fraction_emits_sign = truncation == FloatFractionTruncation::NinePlaceholder;
    match sign_format {
        NumericSignFormat::Default => default_numeric_sign(body, negative, fill_mode),
        NumericSignFormat::Leading => anchored_numeric_sign(body, negative),
        NumericSignFormat::Explicit { implicit_sign } => {
            apply_explicit_numeric_signs(body, negative, fill_mode, implicit_sign)
        }
        NumericSignFormat::Trailing => apply_positioned_numeric_sign(body, negative),
        NumericSignFormat::Minus if fill_mode && negative && truncated_fraction_emits_sign => {
            format!("{body}-")
        }
        NumericSignFormat::Minus => body,
        NumericSignFormat::Sign if fill_mode && truncated_fraction_emits_sign => {
            format!("{body}{}", if negative { '-' } else { '+' })
        }
        NumericSignFormat::Sign => body,
        NumericSignFormat::Plus if fill_mode => {
            if negative {
                format!("-{body}")
            } else if truncated_fraction_emits_sign {
                format!("{body}+")
            } else {
                body
            }
        }
        NumericSignFormat::Plus => default_numeric_sign(body, negative, false),
        NumericSignFormat::Brackets => {
            apply_numeric_sign(body, negative, fill_mode, NumericSignFormat::Brackets)
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FloatFractionTruncation {
    None,
    ZeroPlaceholder,
    NinePlaceholder,
}

impl FloatFractionTruncation {
    fn stops_picture_scan(self, fill_mode: bool) -> bool {
        self != Self::None && (!fill_mode || self == Self::ZeroPlaceholder)
    }
}

fn apply_numeric_sign(
    body: String,
    negative: bool,
    fill_mode: bool,
    sign_format: NumericSignFormat,
) -> String {
    match sign_format {
        NumericSignFormat::Default => default_numeric_sign(body, negative, fill_mode),
        NumericSignFormat::Leading => anchored_numeric_sign(body, negative),
        NumericSignFormat::Explicit { implicit_sign } => {
            apply_explicit_numeric_signs(body, negative, fill_mode, implicit_sign)
        }
        NumericSignFormat::Trailing => apply_positioned_numeric_sign(body, negative),
        NumericSignFormat::Sign => format!("{body}{}", if negative { '-' } else { '+' }),
        NumericSignFormat::Minus => {
            if negative {
                format!("{body}-")
            } else if fill_mode {
                body
            } else {
                format!("{body} ")
            }
        }
        NumericSignFormat::Plus => {
            if fill_mode {
                if negative {
                    format!("-{body}")
                } else {
                    format!("{body}+")
                }
            } else {
                format!(
                    "{}{}",
                    default_numeric_sign(body, negative, false),
                    if negative { ' ' } else { '+' }
                )
            }
        }
        NumericSignFormat::Brackets => apply_numeric_brackets(body, negative, fill_mode),
    }
}

#[derive(Clone, Copy)]
enum NumericSignFormat {
    Default,
    Leading,
    Trailing,
    Explicit { implicit_sign: bool },
    Minus,
    Plus,
    Sign,
    Brackets,
}

fn is_explicit_numeric_sign_marker(character: char) -> bool {
    matches!(
        character,
        NUMERIC_MINUS_MARKER | NUMERIC_PLUS_MARKER | NUMERIC_SIGN_MARKER
    )
}

fn is_numeric_picture_marker(character: char) -> bool {
    is_explicit_numeric_sign_marker(character) || character == NUMERIC_BRACKET_MARKER
}

fn default_numeric_sign(mut body: String, negative: bool, fill_mode: bool) -> String {
    if !negative {
        return if fill_mode { body } else { format!(" {body}") };
    }
    if !fill_mode {
        let leading_spaces = body.bytes().take_while(|byte| *byte == b' ').count();
        if leading_spaces > 0 {
            body.replace_range(leading_spaces - 1..leading_spaces, "-");
            return format!(" {body}");
        }
    }
    format!("-{body}")
}

fn anchored_numeric_sign(mut body: String, negative: bool) -> String {
    let sign = if negative { '-' } else { '+' };
    let leading_spaces = body.bytes().take_while(|byte| *byte == b' ').count();
    body.insert(leading_spaces, sign);
    body
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

fn numeric_text(value: &Value) -> String {
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

#[cfg(test)]
mod number_format_tests {
    use super::{format_pg_number, DecimalValue, Value};

    #[test]
    fn numeric_templates_reserve_postgresql_sign_and_digit_slots() {
        let formatted = |value, template| format_pg_number(&value, template).unwrap();
        assert_eq!(formatted(Value::Float(1234.5), "9999.99"), " 1234.50");
        assert_eq!(formatted(Value::Float(12.3), "9999.99"), "   12.30");
        assert_eq!(formatted(Value::Float(-12.3), "9999.99"), "  -12.30");
        assert_eq!(formatted(Value::Float(12.3), "0000.00"), " 0012.30");
        assert_eq!(formatted(Value::Float(12.3), "FM9999.99"), "12.3");
        assert_eq!(formatted(Value::Float(12.0), "FM9999.99"), "12.");
        assert_eq!(formatted(Value::Float(12_345.6), "9999.99"), " ####.##");
        assert_eq!(formatted(Value::Float(12_345.6), "FM9999.99"), "####.##");
        assert_eq!(formatted(Value::Float(1.0), "FM090"), "001");
        assert_eq!(formatted(Value::Float(12.0), "fm000"), "012");
        assert_eq!(formatted(Value::Float(12.0), "9999."), "   12");
        assert_eq!(formatted(Value::Float(-12.0), "FM9999."), "-12");
        assert_eq!(formatted(Value::Float(0.0), "FM9.9"), "0.");
        assert_eq!(formatted(Value::Float(0.5), "FM9.9"), ".5");
        assert_eq!(formatted(Value::Float(-1.2), "S9"), "-1");
        assert_eq!(formatted(Value::Float(1.2), "9S"), "1+");
        assert_eq!(formatted(Value::Float(12.0), "SFM999"), "+12");
        assert_eq!(formatted(Value::Float(-12.0), "FMS999"), "-12");
        assert_eq!(formatted(Value::Float(12.0), "999FMS"), "12+");
        assert_eq!(formatted(Value::Float(12.0), "9S99"), " +12");
        assert_eq!(formatted(Value::Float(-1.25), "S.9"), ".#-");
        assert_eq!(formatted(Value::Float(-1.25), "9S.9"), "1.2-");
        assert_eq!(formatted(Value::Float(-1.2), "9SG"), "1-");
        assert_eq!(formatted(Value::Float(-1.2), "9MI"), "1-");
        assert_eq!(formatted(Value::Float(-1.2), "9PR"), "<1>");
    }

    #[test]
    fn numeric_and_float_templates_keep_their_postgresql_rounding_rules() {
        let decimal = |value: &str| Value::Decimal(DecimalValue::parse(value).unwrap());
        assert_eq!(format_pg_number(&decimal("2.5"), "9").unwrap(), " 3");
        assert_eq!(format_pg_number(&Value::Float(2.5), "9").unwrap(), " 2");
        assert_eq!(format_pg_number(&decimal("-2.5"), "9").unwrap(), "-3");
        assert_eq!(format_pg_number(&decimal("1.25"), "9.9").unwrap(), " 1.3");
        assert_eq!(
            format_pg_number(&Value::Float(1.25), "9.9").unwrap(),
            " 1.2"
        );
        assert_eq!(format_pg_number(&Value::Float(0.5), "9").unwrap(), " 0");
        assert_eq!(format_pg_number(&decimal("9.99"), "9.").unwrap(), " #.");
        assert_eq!(format_pg_number(&decimal("-9.99"), "9.S").unwrap(), "#.-");
        assert_eq!(format_pg_number(&decimal("-2.5"), "9.S").unwrap(), "3");
        assert_eq!(format_pg_number(&decimal("0.5"), ".9").unwrap(), " .#");
        assert_eq!(format_pg_number(&decimal("0.5"), "FM.9").unwrap(), ".#");
        assert_eq!(format_pg_number(&decimal("-0.04"), "9.9").unwrap(), "  .0");
        assert_eq!(
            format_pg_number(&Value::Float(-0.04), "9.9").unwrap(),
            " -.0"
        );
    }

    #[test]
    fn numeric_templates_render_special_values_like_postgresql() {
        let decimal = |value: &str| Value::Decimal(DecimalValue::parse(value).unwrap());
        assert_eq!(
            format_pg_number(&decimal("NaN"), "99999999.99").unwrap(),
            "      NaN"
        );
        assert_eq!(
            format_pg_number(&decimal("Infinity"), "99999999.99").unwrap(),
            " Infinity"
        );
        assert_eq!(
            format_pg_number(&decimal("-Infinity"), "99999999.99").unwrap(),
            "-Infinity"
        );
        assert_eq!(format_pg_number(&decimal("NaN"), "9.9").unwrap(), " #.#");
        assert_eq!(
            format_pg_number(&decimal("-Infinity"), "99999999.99PR").unwrap(),
            "<Infinity"
        );
        assert_eq!(format_pg_number(&decimal("NaN"), "000MI").unwrap(), "NaN ");
        assert_eq!(
            format_pg_number(&decimal("Infinity"), "000PL").unwrap(),
            " ###+"
        );
    }

    #[test]
    fn numeric_templates_preserve_unrecognized_case_and_quoted_literals() {
        let decimal = |value: &str| Value::Decimal(DecimalValue::parse(value).unwrap());
        assert_eq!(format_pg_number(&decimal("12"), "fM000").unwrap(), "fM 012");
        assert_eq!(format_pg_number(&decimal("-1.2"), "9Mi").unwrap(), "-1Mi");
        assert_eq!(
            format_pg_number(&decimal("12"), r#""USD"000"#).unwrap(),
            "USD 012"
        );
        assert_eq!(
            format_pg_number(&decimal("12"), r#""USD"SFM999"#).unwrap(),
            "USD+12"
        );
    }

    #[test]
    fn numeric_templates_support_postgresql_group_currency_scale_and_suffix_tokens() {
        let decimal = |value: &str| Value::Decimal(DecimalValue::parse(value).unwrap());
        assert_eq!(
            format_pg_number(&decimal("1485"), "9,999").unwrap(),
            " 1,485"
        );
        assert_eq!(
            format_pg_number(&decimal("3148.5"), "9G999D999").unwrap(),
            " 3,148.500"
        );
        assert_eq!(format_pg_number(&decimal("485"), "L999").unwrap(), "$ 485");
        assert_eq!(format_pg_number(&decimal("12"), "FM00L").unwrap(), "12$");
        assert_eq!(format_pg_number(&decimal("12"), "FM00B").unwrap(), "12");
        assert_eq!(format_pg_number(&decimal("12"), "FM00C").unwrap(), "12");
        assert_eq!(format_pg_number(&decimal("12.45"), "99V9").unwrap(), " 125");
        assert_eq!(
            format_pg_number(&decimal("482"), "999th").unwrap(),
            " 482nd"
        );
        assert_eq!(format_pg_number(&decimal("0.5"), "FM00TH").unwrap(), "01ST");
        assert_eq!(format_pg_number(&decimal("12"), "SP").unwrap(), "");
    }

    #[test]
    fn numeric_templates_support_postgresql_roman_and_scientific_notation() {
        let decimal = |value: &str| Value::Decimal(DecimalValue::parse(value).unwrap());
        assert_eq!(
            format_pg_number(&decimal("485"), "RN").unwrap(),
            "        CDLXXXV"
        );
        assert_eq!(format_pg_number(&decimal("5.2"), "FMRN").unwrap(), "V");
        assert_eq!(
            format_pg_number(&decimal("0"), "RN").unwrap(),
            "###############"
        );
        assert_eq!(
            format_pg_number(&decimal("0.0004859"), "9.99EEEE").unwrap(),
            " 4.86e-04"
        );
        assert_eq!(
            format_pg_number(&decimal("9.99"), "9.9EEEE").unwrap(),
            " 10.0e+00"
        );
        assert_eq!(
            format_pg_number(&decimal("NaN"), "9.99EEEE").unwrap(),
            " #.######"
        );
        assert_eq!(
            format_pg_number(&decimal("-Infinity"), "9.99EEEE").unwrap(),
            " #.######"
        );
        assert_eq!(
            format_pg_number(&Value::Float(0.5), "RN").unwrap(),
            "###############"
        );
        assert_eq!(format_pg_number(&Value::Float(2.5), "FMRN").unwrap(), "II");
        assert_eq!(
            format_pg_number(&Value::Float(9.99), "9.9EEEE").unwrap(),
            " 1.0e+01"
        );
        assert_eq!(
            format_pg_number(&Value::Float(-0.0), "9.9EEEE").unwrap(),
            "-0.0e+00"
        );
    }

    #[test]
    fn float_templates_apply_postgresql_precision_before_format_processing() {
        let formatted = |value, template| format_pg_number(&Value::Float(value), template).unwrap();
        assert_eq!(formatted(1e20, "9.9"), " #.");
        assert_eq!(formatted(-1e20, "9.9"), "-#.");
        assert_eq!(formatted(1e20, "9.9MI"), "#.");
        assert_eq!(formatted(-1e20, "9.9MI"), "#.");
        assert_eq!(formatted(1e20, "9.9PL"), " #.");
        assert_eq!(formatted(-1e20, "9.9PL"), "-#.");
        assert_eq!(formatted(1e20, "9.9S"), "#.+");
        assert_eq!(formatted(-1e20, "9.9S"), "#.-");
        assert_eq!(formatted(1e20, "9.9PR"), " #. ");
        assert_eq!(formatted(-1e20, "9.9PR"), "<#.>");
        assert_eq!(formatted(1e20, "FM9.9PL"), "#.+");
        assert_eq!(formatted(-1e20, "FM9.9MI"), "#.-");
        assert_eq!(formatted(1e20, "FM9.0PL"), "#.");
        assert_eq!(formatted(1e20, "FM9.9SG"), "#.+");
        assert_eq!(formatted(1e20, "9.9MIPL"), "#.");
        assert_eq!(formatted(1e20, "FM9.9MIPL"), "#.+");
        assert_eq!(formatted(1e20, "FM09.90PL"), "##.");
        assert_eq!(formatted(-1e20, "FM09.90MI"), "##.");
        assert_eq!(formatted(-1e20, "FM09.90SG"), "##.");
        assert_eq!(
            formatted(-12_345_678_901_234.0, "FM99999999999999.09MI"),
            "12345678901234.0-"
        );
        assert_eq!(
            formatted(-12_345_678_901_234.0, "FM99999999999999.90MI"),
            "12345678901234.0"
        );
        assert_eq!(
            formatted(12_345_678_901_234.0, "99999999999999.9999"),
            " 12345678901234.0"
        );
    }
}
