//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Locale-independent numeric and Boolean spellings accepted by relation options.

pub(super) fn boolean(value: &str) -> bool {
    value == value.trim()
        && crate::expr::cast_value(&uqa_core::Value::Str(value.into()), "boolean").is_ok()
}

pub(super) fn integer(value: &str) -> Option<i32> {
    let text = value.trim_matches(|c: char| c.is_ascii_whitespace());
    let (negative, body) = sign(text);
    let (radix, digits) =
        if let Some(hex) = body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
            (16, hex)
        } else if body.starts_with('0') {
            (8, body)
        } else {
            (10, body)
        };
    let count = digits
        .bytes()
        .take_while(|byte| char::from(*byte).is_digit(radix))
        .count();
    let rest = &digits[count..];
    let integral = i64::from_str_radix(&digits[..count], radix);
    let overflow = integral.as_ref().err().is_some_and(|error| {
        matches!(
            error.kind(),
            std::num::IntErrorKind::PosOverflow | std::num::IntErrorKind::NegOverflow
        )
    });
    let number = if rest.starts_with(['.', 'e', 'E']) || overflow {
        real(text)?
    } else if count == 0 || !rest.is_empty() {
        return None;
    } else {
        let integer = integral.ok()?;
        if negative {
            -(integer as f64)
        } else {
            integer as f64
        }
    };
    let rounded = number.round_ties_even();
    (rounded >= f64::from(i32::MIN) && rounded <= f64::from(i32::MAX)).then_some(rounded as i32)
}

pub(super) fn real(value: &str) -> Option<f64> {
    let text = value.trim_matches(|c: char| c.is_ascii_whitespace());
    let (negative, body) = sign(text);
    if let Some(hex) = body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
        return hexadecimal(hex).map(|value| if negative { -value } else { value });
    }
    let parsed = text.parse::<f64>().ok()?;
    if parsed.is_nan() {
        return None;
    }
    if parsed.is_infinite() {
        return (body.eq_ignore_ascii_case("inf") || body.eq_ignore_ascii_case("infinity"))
            .then_some(parsed);
    }
    let nonzero = body
        .split(['e', 'E'])
        .next()?
        .bytes()
        .any(|digit| matches!(digit, b'1'..=b'9'));
    if parsed == 0.0 && nonzero {
        return None;
    }
    if parsed != 0.0 && parsed.abs() <= f64::MIN_POSITIVE {
        let exact = uqa_core::DecimalValue::from_f64_exact(parsed.abs());
        let decimal = uqa_core::DecimalValue::parse(body)?;
        let normal = uqa_core::DecimalValue::from_f64_exact(f64::MIN_POSITIVE);
        if decimal < normal && decimal != exact {
            return None;
        }
    }
    Some(parsed)
}

fn sign(text: &str) -> (bool, &str) {
    if let Some(rest) = text.strip_prefix('-') {
        (true, rest)
    } else {
        (false, text.strip_prefix('+').unwrap_or(text))
    }
}

fn hexadecimal(text: &str) -> Option<f64> {
    let (mantissa, exponent) = text.split_once(['p', 'P']).map_or((text, "0"), |pair| pair);
    let exponent = exponent_value(exponent)?;
    let mut point = false;
    let mut fraction = 0_i64;
    let mut digits = 0_usize;
    let mut kept = 0_u32;
    let mut high = 0_u64;
    let mut tail = 0_i64;
    let mut sticky = false;
    let mut trailing = 0_i64;
    let mut low = 0_u32;
    for digit in mantissa.chars() {
        if digit == '.' && !point {
            point = true;
            continue;
        }
        let digit = digit.to_digit(16)?;
        digits += 1;
        if point {
            fraction = fraction.checked_add(1)?;
        }
        if digit == 0 {
            trailing = trailing.checked_add(1)?;
        } else {
            trailing = 0;
            low = digit.trailing_zeros();
        }
        if high == 0 && digit == 0 {
            continue;
        }
        if kept < 16 {
            high = (high << 4) | u64::from(digit);
            kept += 1;
        } else {
            tail = tail.checked_add(1)?;
            sticky |= digit != 0;
        }
    }
    if digits == 0 {
        return None;
    }
    if high == 0 {
        return Some(0.0);
    }
    let scale = exponent.checked_sub(fraction.checked_mul(4)?)?;
    let power = scale.checked_add(tail.checked_mul(4)?)?;
    let rounded = (high | u64::from(sticky)) as f64;
    let value = libm::scalbn(
        rounded,
        power.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
    );
    if !value.is_finite() || value == 0.0 {
        return None;
    }
    let lowest_bit = scale
        .checked_add(trailing.checked_mul(4)?)?
        .checked_add(i64::from(low))?;
    let highest_bit = power.checked_add(i64::from(63 - high.leading_zeros()))?;
    if highest_bit < -1022 && lowest_bit < -1074 {
        return None;
    }
    Some(value)
}

fn exponent_value(text: &str) -> Option<i64> {
    let (negative, digits) = sign(text);
    if digits.is_empty() {
        return None;
    }
    let mut value = 0_i64;
    for digit in digits.bytes() {
        if !digit.is_ascii_digit() {
            return None;
        }
        // Exponents outside this range cannot be offset by an addressable mantissa.
        value = value
            .saturating_mul(10)
            .saturating_add(i64::from(digit - b'0'))
            .min(i64::MAX / 4);
    }
    Some(if negative { -value } else { value })
}
