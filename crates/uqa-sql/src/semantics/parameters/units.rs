//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Integer parameter values and their units, read and shown as `PostgreSQL`'s `parse_int`, `convert_to_base_unit` and `convert_int_from_base_unit` do: C `strtol` and `strtod` number syntax, a unit of up to three characters, rounding to the nearest multiple of the next smaller unit, and display in the greatest unit that divides the value.

/// The base unit of an integer parameter (`GUC_UNIT_*`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParameterUnit {
    Bytes,
    Kilobytes,
    Megabytes,
    Milliseconds,
    Seconds,
    Minutes,
}

const MEMORY_UNITS_HINT: &str =
    "Valid units for this parameter are \"B\", \"kB\", \"MB\", \"GB\", and \"TB\".";
const TIME_UNITS_HINT: &str =
    "Valid units for this parameter are \"us\", \"ms\", \"s\", \"min\", \"h\", and \"d\".";
const INTEGER_RANGE_HINT: &str = "Value exceeds integer range.";
const MAX_UNIT_LENGTH: usize = 3;
const KIB: f64 = 1024.0;

impl ParameterUnit {
    /// The name that `pg_settings.unit` and range errors report.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Bytes => "B",
            Self::Kilobytes => "kB",
            Self::Megabytes => "MB",
            Self::Milliseconds => "ms",
            Self::Seconds => "s",
            Self::Minutes => "min",
        }
    }

    const fn hint(self) -> &'static str {
        match self {
            Self::Bytes | Self::Kilobytes | Self::Megabytes => MEMORY_UNITS_HINT,
            Self::Milliseconds | Self::Seconds | Self::Minutes => TIME_UNITS_HINT,
        }
    }

    /// The units a value may name and how many base units each holds, from the greatest to the smallest, as `memory_unit_conversion_table` and `time_unit_conversion_table` list them.
    fn conversions(self) -> [(&'static str, f64); 6] {
        match self {
            Self::Bytes => [
                ("TB", KIB * KIB * KIB * KIB),
                ("GB", KIB * KIB * KIB),
                ("MB", KIB * KIB),
                ("kB", KIB),
                ("B", 1.0),
                ("", 0.0),
            ],
            Self::Kilobytes => [
                ("TB", KIB * KIB * KIB),
                ("GB", KIB * KIB),
                ("MB", KIB),
                ("kB", 1.0),
                ("B", 1.0 / KIB),
                ("", 0.0),
            ],
            Self::Megabytes => [
                ("TB", KIB * KIB),
                ("GB", KIB),
                ("MB", 1.0),
                ("kB", 1.0 / KIB),
                ("B", 1.0 / (KIB * KIB)),
                ("", 0.0),
            ],
            Self::Milliseconds => [
                ("d", 86_400_000.0),
                ("h", 3_600_000.0),
                ("min", 60_000.0),
                ("s", 1000.0),
                ("ms", 1.0),
                ("us", 1.0 / 1000.0),
            ],
            Self::Seconds => [
                ("d", 86_400.0),
                ("h", 3600.0),
                ("min", 60.0),
                ("s", 1.0),
                ("ms", 1.0 / 1000.0),
                ("us", 1.0 / (1000.0 * 1000.0)),
            ],
            Self::Minutes => [
                ("d", 1440.0),
                ("h", 60.0),
                ("min", 1.0),
                ("s", 1.0 / 60.0),
                ("ms", 1.0 / (1000.0 * 60.0)),
                ("us", 1.0 / (1000.0 * 1000.0 * 60.0)),
            ],
        }
    }
}

/// Read an integer value in base units. The error carries the hint `PostgreSQL` adds to `invalid value for parameter`, if any.
pub(super) fn parse_integer(
    raw: &str,
    unit: Option<ParameterUnit>,
) -> Result<i32, Option<&'static str>> {
    let bytes = raw.as_bytes();
    let (integer, integer_end, overflow) = c_strtol(bytes);
    let (mut value, mut end) = (integer as f64, integer_end);
    if overflow || matches!(bytes.get(integer_end), Some(b'.' | b'e' | b'E')) {
        let (double, double_end, out_of_range) = c_strtod(bytes);
        if double_end == 0 || out_of_range {
            return Err(None);
        }
        (value, end) = (double, double_end);
    } else if integer_end == 0 {
        return Err(None);
    }
    if value.is_nan() {
        return Err(None);
    }
    end = skip_c_space(bytes, end);
    if end < bytes.len() {
        let unit = unit.ok_or(None)?;
        value = convert_to_base_unit(value, &bytes[end..], unit).ok_or(Some(unit.hint()))?;
    }
    let value = value.round_ties_even();
    if value > f64::from(i32::MAX) || value < f64::from(i32::MIN) {
        return Err(Some(INTEGER_RANGE_HINT));
    }
    Ok(value as i32)
}

/// Show an integer value in the greatest unit that divides it.
pub(super) fn show_integer(value: i64, unit: Option<ParameterUnit>) -> String {
    let Some(unit) = unit.filter(|_| value > 0) else {
        return value.to_string();
    };
    for (name, multiplier) in unit.conversions() {
        if name.is_empty() {
            break;
        }
        if multiplier <= 1.0 || value % (multiplier as i64) == 0 {
            let converted = (value as f64 / multiplier).round_ties_even() as i64;
            return format!("{converted}{name}");
        }
    }
    unreachable!("every unit lists its base unit")
}

/// Convert `value` given in the unit that `text` names to base units, rounding a fraction to the nearest multiple of the next smaller unit; `None` for an unknown unit or text after it.
fn convert_to_base_unit(value: f64, text: &[u8], unit: ParameterUnit) -> Option<f64> {
    let length = text
        .iter()
        .take(MAX_UNIT_LENGTH)
        .take_while(|byte| !is_c_space(**byte))
        .count();
    if skip_c_space(text, length) != text.len() {
        return None;
    }
    let conversions = unit.conversions();
    let index = conversions
        .iter()
        .position(|(name, _)| !name.is_empty() && name.as_bytes() == &text[..length])?;
    let mut converted = value * conversions[index].1;
    if let Some((next, multiplier)) = conversions.get(index + 1) {
        if !next.is_empty() {
            converted = (converted / multiplier).round_ties_even() * multiplier;
        }
    }
    Some(converted)
}

/// The white space of the C locale's `isspace`.
pub(super) fn is_c_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

fn skip_c_space(bytes: &[u8], mut index: usize) -> usize {
    while bytes.get(index).copied().is_some_and(is_c_space) {
        index += 1;
    }
    index
}

/// C `strtol(text, &end, 0)` on a 64-bit `long`: the value, the offset where the conversion stopped (0 when nothing was converted) and whether the value overflowed. A leading `0x` selects hexadecimal and a leading `0` octal.
fn c_strtol(bytes: &[u8]) -> (i64, usize, bool) {
    let mut index = skip_c_space(bytes, 0);
    let negative = match bytes.get(index) {
        Some(b'-') => {
            index += 1;
            true
        }
        Some(b'+') => {
            index += 1;
            false
        }
        _ => false,
    };
    let hexadecimal = matches!(bytes.get(index..index + 2), Some(b"0x" | b"0X"))
        && bytes.get(index + 2).is_some_and(u8::is_ascii_hexdigit);
    let radix = if hexadecimal {
        index += 2;
        16
    } else if bytes.get(index) == Some(&b'0') {
        8
    } else {
        10
    };
    let start = index;
    let mut magnitude: u128 = 0;
    let mut overflow = false;
    while let Some(digit) = bytes
        .get(index)
        .and_then(|byte| char::from(*byte).to_digit(radix))
    {
        magnitude = magnitude * u128::from(radix) + u128::from(digit);
        if magnitude > u128::from(i64::MAX.unsigned_abs()) + 1 {
            magnitude = u128::from(i64::MAX.unsigned_abs()) + 1;
            overflow = true;
        }
        index += 1;
    }
    if index == start {
        return (0, 0, false);
    }
    let value = if negative {
        i64::try_from(-(magnitude as i128)).unwrap_or_else(|_| {
            overflow = true;
            i64::MIN
        })
    } else {
        i64::try_from(magnitude).unwrap_or_else(|_| {
            overflow = true;
            i64::MAX
        })
    };
    (value, index, overflow)
}

/// C `strtod`: the value, the offset where the conversion stopped (0 when nothing was converted) and whether the value overflowed or underflowed (`ERANGE`). Reads decimal and hexadecimal numbers, `inf`, `infinity` and `nan`.
pub(super) fn c_strtod(bytes: &[u8]) -> (f64, usize, bool) {
    let mut index = skip_c_space(bytes, 0);
    let sign_start = index;
    let negative = match bytes.get(index) {
        Some(b'-') => {
            index += 1;
            true
        }
        Some(b'+') => {
            index += 1;
            false
        }
        _ => false,
    };
    let signed = |value: f64| if negative { -value } else { value };
    let rest = &bytes[index..];
    let starts_with =
        |word: &[u8]| rest.len() >= word.len() && rest[..word.len()].eq_ignore_ascii_case(word);
    if starts_with(b"infinity") {
        return (signed(f64::INFINITY), index + 8, false);
    }
    if starts_with(b"inf") {
        return (signed(f64::INFINITY), index + 3, false);
    }
    if starts_with(b"nan") {
        let mut end = index + 3;
        if bytes.get(end) == Some(&b'(') {
            let close = bytes[end + 1..]
                .iter()
                .position(|byte| !(byte.is_ascii_alphanumeric() || *byte == b'_'));
            if let Some(offset) = close {
                if bytes[end + 1 + offset] == b')' {
                    end += offset + 2;
                }
            }
        }
        return (f64::NAN, end, false);
    }
    if matches!(rest.get(..2), Some(b"0x" | b"0X")) {
        if let Some((value, end, nonzero)) = hexadecimal_float(bytes, index + 2) {
            return finish_strtod(signed(value), end, nonzero);
        }
    }
    let mut end = index;
    let mut nonzero = false;
    let mut digits = 0;
    while let Some(byte) = bytes.get(end).filter(|byte| byte.is_ascii_digit()) {
        nonzero |= *byte != b'0';
        digits += 1;
        end += 1;
    }
    if bytes.get(end) == Some(&b'.') {
        end += 1;
        while let Some(byte) = bytes.get(end).filter(|byte| byte.is_ascii_digit()) {
            nonzero |= *byte != b'0';
            digits += 1;
            end += 1;
        }
    }
    if digits == 0 {
        return (0.0, 0, false);
    }
    if matches!(bytes.get(end), Some(b'e' | b'E')) {
        let mut exponent = end + 1;
        if matches!(bytes.get(exponent), Some(b'+' | b'-')) {
            exponent += 1;
        }
        if bytes.get(exponent).is_some_and(u8::is_ascii_digit) {
            end = exponent;
            while bytes.get(end).is_some_and(u8::is_ascii_digit) {
                end += 1;
            }
        }
    }
    let text = std::str::from_utf8(&bytes[sign_start..end]).expect("ASCII number");
    let value = text.parse::<f64>().expect("C decimal number syntax");
    finish_strtod(value, end, nonzero)
}

fn finish_strtod(value: f64, end: usize, nonzero: bool) -> (f64, usize, bool) {
    let out_of_range = value.is_infinite() || (nonzero && value.abs() < f64::MIN_POSITIVE);
    (value, end, out_of_range)
}

/// The hexadecimal mantissa and optional binary exponent after `0x`: the magnitude, the offset after it and whether any digit was nonzero; `None` when no hexadecimal digit follows, which leaves `strtod` to read the `0` alone.
fn hexadecimal_float(bytes: &[u8], start: usize) -> Option<(f64, usize, bool)> {
    let mut index = start;
    let mut mantissa: u64 = 0;
    let mut exponent: i64 = 0;
    let mut digits = 0;
    let mut nonzero = false;
    let mut accumulate = |digit: u32, fraction: bool, exponent: &mut i64| {
        nonzero |= digit != 0;
        if mantissa >> 60 == 0 {
            mantissa = mantissa << 4 | u64::from(digit);
            if fraction {
                *exponent -= 4;
            }
        } else if !fraction {
            *exponent += 4;
        }
    };
    while let Some(digit) = bytes
        .get(index)
        .and_then(|byte| char::from(*byte).to_digit(16))
    {
        accumulate(digit, false, &mut exponent);
        digits += 1;
        index += 1;
    }
    if bytes.get(index) == Some(&b'.') {
        let mut fraction = index + 1;
        let mut fraction_digits = 0;
        while let Some(digit) = bytes
            .get(fraction)
            .and_then(|byte| char::from(*byte).to_digit(16))
        {
            accumulate(digit, true, &mut exponent);
            fraction_digits += 1;
            fraction += 1;
        }
        if digits + fraction_digits > 0 {
            index = fraction;
            digits += fraction_digits;
        }
    }
    if digits == 0 {
        return None;
    }
    if matches!(bytes.get(index), Some(b'p' | b'P')) {
        let mut cursor = index + 1;
        let negative = match bytes.get(cursor) {
            Some(b'-') => {
                cursor += 1;
                true
            }
            Some(b'+') => {
                cursor += 1;
                false
            }
            _ => false,
        };
        if bytes.get(cursor).is_some_and(u8::is_ascii_digit) {
            let mut binary: i64 = 0;
            while let Some(byte) = bytes.get(cursor).filter(|byte| byte.is_ascii_digit()) {
                binary = binary
                    .saturating_mul(10)
                    .saturating_add(i64::from(byte - b'0'));
                cursor += 1;
            }
            exponent = exponent.saturating_add(if negative { -binary } else { binary });
            index = cursor;
        }
    }
    // Scale in steps that stay within the normal range, so that a large mantissa with a very negative exponent does not vanish before the multiplication.
    let mut remaining = exponent.clamp(-2200, 2200);
    let mut value = mantissa as f64;
    while remaining > 1000 {
        value *= 2f64.powi(1000);
        remaining -= 1000;
    }
    while remaining < -1000 {
        value *= 2f64.powi(-1000);
        remaining += 1000;
    }
    let remaining = i32::try_from(remaining).expect("exponent within one step");
    Some((value * 2f64.powi(remaining), index, nonzero))
}

#[cfg(test)]
mod tests;
