//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL`'s base-10000 numeric Datum, including short and special headers.

use crate::catalog::node_tree::invalid;
use crate::SQLError;
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionString, ProductionVec},
    DecimalValue,
};

pub(crate) fn encode(value: &DecimalValue) -> Result<Vec<u8>, SQLError> {
    encode_with_control(value, &ProductionControl::uncontrolled()).map(|value| {
        value
            .into_uncontrolled()
            .expect("ordinary numeric Datum encoding")
    })
}

/// Preserve the original producer allowance while encoding the selected numeric representation.
pub(crate) fn encode_with_control(
    value: &DecimalValue,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<u8>>, SQLError> {
    control.check()?;
    let mut bytes = ProductionVec::new(*control);
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
        push_word(&mut bytes, header)?;
        return Ok(bytes.finish()?);
    }
    let text = value.to_sql_string_with_control(control)?;
    let text = text.strip_prefix('-').unwrap_or(&text);
    let (integer, fraction) = text.split_once('.').unwrap_or((text, ""));
    let scale = u16::try_from(fraction.len()).map_err(|_| invalid("numeric scale overflow"))?;
    let groups = DecimalGroups::new(integer, fraction);
    let (first, end) = groups.nonzero_bounds(control)?;
    let weight = if first == end {
        0
    } else {
        i32::try_from(groups.integer_groups).unwrap_or(i32::MAX)
            - i32::try_from(first).unwrap_or(i32::MAX)
            - 1
    };
    let weight = i16::try_from(weight).map_err(|_| invalid("numeric weight overflow"))?;
    if (-64..=63).contains(&weight) && scale <= 63 {
        let header = 0x8000
            | if value.is_negative() { 0x2000 } else { 0 }
            | (scale << 7)
            | (u16::from_le_bytes(weight.to_le_bytes()) & 0x7f);
        push_word(&mut bytes, header)?;
    } else {
        if scale > 0x3fff {
            return Err(invalid("numeric scale overflow"));
        }
        let header = scale | if value.is_negative() { 0x4000 } else { 0 };
        push_word(&mut bytes, header)?;
        push_word(&mut bytes, u16::from_le_bytes(weight.to_le_bytes()))?;
    }
    bytes.reserve((end - first).saturating_mul(2))?;
    for index in first..end {
        control.check()?;
        push_word(&mut bytes, groups.at(index))?;
    }
    Ok(bytes.finish()?)
}

fn push_word(bytes: &mut ProductionVec<'_, u8>, value: u16) -> Result<(), SQLError> {
    for byte in value.to_le_bytes() {
        bytes.push_copy(byte)?;
    }
    Ok(())
}

/// Borrow virtual zero-padded groups instead of allocating a second decimal string and a digit vector.
struct DecimalGroups<'a> {
    integer: &'a [u8],
    fraction: &'a [u8],
    integer_groups: usize,
}

impl<'a> DecimalGroups<'a> {
    fn new(integer: &'a str, fraction: &'a str) -> Self {
        Self {
            integer: integer.as_bytes(),
            fraction: fraction.as_bytes(),
            integer_groups: integer.len().div_ceil(4),
        }
    }

    fn at(&self, index: usize) -> u16 {
        let leading = self.integer_groups * 4 - self.integer.len();
        (index * 4..index * 4 + 4).fold(0, |group, position| {
            let digit = position.checked_sub(leading).map_or(0, |position| {
                self.integer
                    .get(position)
                    .or_else(|| self.fraction.get(position - self.integer.len()))
                    .map_or(0, |digit| digit - b'0')
            });
            group * 10 + u16::from(digit)
        })
    }

    fn nonzero_bounds(&self, control: &ProductionControl<'_>) -> Result<(usize, usize), SQLError> {
        let count = self.integer_groups + self.fraction.len().div_ceil(4);
        let mut first = count;
        let mut end = count;
        for index in 0..count {
            control.check()?;
            if self.at(index) != 0 {
                first = first.min(index);
                end = index + 1;
            }
        }
        Ok((first, end))
    }
}

pub(crate) fn decode(bytes: &[u8]) -> Result<String, SQLError> {
    decode_with_control(bytes, &ProductionControl::uncontrolled()).map(|value| {
        value
            .into_uncontrolled()
            .expect("ordinary numeric Datum output")
    })
}

pub(crate) fn decode_with_control(
    bytes: &[u8],
    control: &ProductionControl<'_>,
) -> Result<Produced<String>, SQLError> {
    control.check()?;
    if bytes.len() < 2 || !bytes.len().is_multiple_of(2) {
        return Err(invalid("invalid numeric Datum length"));
    }
    let header = u16::from_le_bytes([bytes[0], bytes[1]]);
    if header & 0xc000 == 0xc000 {
        if bytes.len() != 2 {
            return Err(invalid("invalid special numeric Datum length"));
        }
        return match header {
            0xc000 => Ok(control.copy_text("NaN")?),
            0xd000 => Ok(control.copy_text("Infinity")?),
            0xf000 => Ok(control.copy_text("-Infinity")?),
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
    let digits = bytes[offset..].as_chunks::<2>().0;
    for digit in digits {
        control.check()?;
        if u16::from_le_bytes(*digit) >= 10_000 {
            return Err(invalid("invalid numeric base-10000 digit"));
        }
    }
    let digit_at = |position: i32| {
        usize::try_from(weight - position)
            .ok()
            .and_then(|index| digits.get(index))
            .copied()
            .map(u16::from_le_bytes)
            .unwrap_or(0)
    };
    let mut text = ProductionString::new(*control);
    if negative {
        text.push('-')?;
    }
    if weight < 0 {
        text.push('0')?;
    } else {
        text.push_str(&control.format(format_args!("{}", digit_at(weight)))?)?;
        for position in (0..weight).rev() {
            text.push_str(&control.format(format_args!("{:04}", digit_at(position)))?)?;
        }
    }
    if scale > 0 {
        text.push('.')?;
        let mut remaining = usize::from(scale);
        for group in 1..=i32::from(scale.div_ceil(4)) {
            let digits = control.format(format_args!("{:04}", digit_at(-group)))?;
            let count = remaining.min(4);
            text.push_str(&digits[..count])?;
            remaining -= count;
        }
    }
    Ok(text.finish()?)
}

#[cfg(test)]
mod tests;
