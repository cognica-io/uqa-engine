//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL`'s base-10000 numeric Datum, including short and special headers.

use crate::catalog::node_tree::invalid;
use crate::SQLError;
use uqa_core::DecimalValue;

pub(super) fn encode(value: &DecimalValue) -> Result<Vec<u8>, SQLError> {
    let special = if value.is_nan() {
        Some(0xc000_u16)
    } else if value.is_positive_infinity() {
        Some(0xd000)
    } else if value.is_negative_infinity() {
        Some(0xf000)
    } else {
        None
    };
    if let Some(header) = special {
        return Ok(header.to_le_bytes().to_vec());
    }
    let text = value.to_sql_string();
    let text = text.strip_prefix('-').unwrap_or(&text);
    let (integer, fraction) = text.split_once('.').unwrap_or((text, ""));
    let scale = u16::try_from(fraction.len()).map_err(|_| invalid("numeric scale overflow"))?;
    let integer_groups = integer.len().div_ceil(4);
    let padded = format!(
        "{}{}{}{}",
        "0".repeat(integer_groups * 4 - integer.len()),
        integer,
        fraction,
        "0".repeat((4 - fraction.len() % 4) % 4),
    );
    let digits = padded
        .as_bytes()
        .chunks_exact(4)
        .map(|group| {
            group
                .iter()
                .fold(0_u16, |value, digit| value * 10 + u16::from(digit - b'0'))
        })
        .collect::<Vec<_>>();
    let first = digits
        .iter()
        .position(|digit| *digit != 0)
        .unwrap_or(digits.len());
    let end = digits
        .iter()
        .rposition(|digit| *digit != 0)
        .map_or(first, |index| index + 1);
    let weight = if first == end {
        0
    } else {
        i32::try_from(integer_groups).unwrap_or(i32::MAX)
            - i32::try_from(first).unwrap_or(i32::MAX)
            - 1
    };
    let weight = i16::try_from(weight).map_err(|_| invalid("numeric weight overflow"))?;
    let mut bytes = Vec::new();
    if (-64..=63).contains(&weight) && scale <= 63 {
        let header = 0x8000
            | if value.is_negative() { 0x2000 } else { 0 }
            | (scale << 7)
            | (u16::from_le_bytes(weight.to_le_bytes()) & 0x7f);
        bytes.extend(header.to_le_bytes());
    } else {
        if scale > 0x3fff {
            return Err(invalid("numeric scale overflow"));
        }
        let header = scale | if value.is_negative() { 0x4000 } else { 0 };
        bytes.extend(header.to_le_bytes());
        bytes.extend(weight.to_le_bytes());
    }
    bytes.extend(
        digits[first..end]
            .iter()
            .flat_map(|digit| digit.to_le_bytes()),
    );
    Ok(bytes)
}

pub(crate) fn decode(bytes: &[u8]) -> Result<String, SQLError> {
    if bytes.len() < 2 || !bytes.len().is_multiple_of(2) {
        return Err(invalid("invalid numeric Datum length"));
    }
    let header = u16::from_le_bytes([bytes[0], bytes[1]]);
    if header & 0xc000 == 0xc000 {
        if bytes.len() != 2 {
            return Err(invalid("invalid special numeric Datum length"));
        }
        return match header {
            0xc000 => Ok("NaN".into()),
            0xd000 => Ok("Infinity".into()),
            0xf000 => Ok("-Infinity".into()),
            _ => Err(invalid("invalid special numeric Datum")),
        };
    }
    let (negative, scale, weight, offset) = if header & 0xc000 == 0x8000 {
        let weight = i32::from(header & 0x3f) - if header & 0x40 != 0 { 64 } else { 0 };
        (header & 0x2000 != 0, (header & 0x1f80) >> 7, weight, 2)
    } else {
        if bytes.len() < 4 {
            return Err(invalid("truncated numeric weight"));
        }
        (
            header & 0x4000 != 0,
            header & 0x3fff,
            i32::from(i16::from_le_bytes([bytes[2], bytes[3]])),
            4,
        )
    };
    let digits = bytes[offset..]
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect::<Vec<_>>();
    if digits.iter().any(|digit| *digit >= 10_000) {
        return Err(invalid("invalid numeric base-10000 digit"));
    }
    let digit_at = |position: i32| {
        usize::try_from(weight - position)
            .ok()
            .and_then(|index| digits.get(index))
            .copied()
            .unwrap_or(0)
    };
    let mut text = String::new();
    if negative {
        text.push('-');
    }
    if weight < 0 {
        text.push('0');
    } else {
        text.push_str(&digit_at(weight).to_string());
        for position in (0..weight).rev() {
            use std::fmt::Write;
            write!(text, "{:04}", digit_at(position)).expect("writing a String");
        }
    }
    if scale > 0 {
        text.push('.');
        let end = text.len() + usize::from(scale);
        for group in 1..=i32::from(scale.div_ceil(4)) {
            use std::fmt::Write;
            write!(text, "{:04}", digit_at(-group)).expect("writing a String");
        }
        text.truncate(end);
    }
    Ok(text)
}
