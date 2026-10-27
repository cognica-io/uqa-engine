//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL bit-string input, length constraints and output over Core's packed values.

use crate::error::{Result, SQLError};
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionString, ProductionVec},
    BitStringValue,
};

mod integer;
pub use integer::{from_integer_with_control, to_bigint, to_integer};

/// `PostgreSQL`'s largest admitted bit-string payload length, independent of a declared column modifier.
const MAX_BIT_LENGTH: u32 = i32::MAX as u32 - 7;
const MAX_DECLARED_LENGTH: u32 = 83_886_080;

/// Fixed and varying declarations share a value representation, but have different input and assignment constraints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BitStringKind {
    Fixed,
    Varying,
}

impl BitStringKind {
    /// Validate the one integer length supplied by the declaration owner. The caller owns modifier arity and array-shape validation.
    pub fn validate_length(self, length: i64) -> Result<u32> {
        let name = match self {
            Self::Fixed => "bit",
            Self::Varying => "varbit",
        };
        if length < 1 {
            return Err(error(
                "22023",
                format!("length for type {name} must be at least 1"),
            ));
        }
        if length > i64::from(MAX_DECLARED_LENGTH) {
            return Err(error(
                "22023",
                format!("length for type {name} cannot exceed {MAX_DECLARED_LENGTH}"),
            ));
        }
        Ok(length as u32)
    }

    pub(crate) fn check_length(self, length: u32, modifier: i32) -> Result<()> {
        if modifier <= 0 {
            return Ok(());
        }
        match self {
            Self::Fixed if length != modifier as u32 => Err(error(
                "22026",
                format!("bit string length {length} does not match type bit({modifier})"),
            )),
            Self::Varying if length > modifier as u32 => Err(error(
                "22001",
                format!("bit string too long for type bit varying({modifier})"),
            )),
            _ => Ok(()),
        }
    }
}

/// Interpret binary or hexadecimal input. An input function checks the declared length before inspecting digits; explicit casts apply their modifier after unconstrained input instead.
pub fn input_with_control(
    text: &str,
    kind: BitStringKind,
    modifier: i32,
    control: &ProductionControl<'_>,
) -> Result<Produced<BitStringValue>> {
    control.check()?;
    let (digits, bits_per_digit) = match text.as_bytes().first() {
        Some(b'b' | b'B') => (&text[1..], 1),
        Some(b'x' | b'X') => (&text[1..], 4),
        _ => (text, 1),
    };
    let length = digits
        .len()
        .checked_mul(bits_per_digit)
        .and_then(|length| u32::try_from(length).ok())
        .filter(|length| *length <= MAX_BIT_LENGTH)
        .ok_or_else(|| {
            error(
                "54000",
                format!("bit string length exceeds the maximum allowed ({MAX_BIT_LENGTH})"),
            )
        })?;
    kind.check_length(length, modifier)?;
    let mut bytes = ProductionVec::new(*control);
    bytes.reserve(length.div_ceil(8) as usize)?;
    let digits_per_byte = 8 / bits_per_digit;
    for (index, chunk) in digits.as_bytes().chunks(digits_per_byte).enumerate() {
        control.check()?;
        let mut packed = 0_u8;
        for (position, digit) in chunk.iter().copied().enumerate() {
            let value = match digit {
                b'0'..=b'1' => digit - b'0',
                b'2'..=b'9' if bits_per_digit == 4 => digit - b'0',
                b'a'..=b'f' if bits_per_digit == 4 => digit - b'a' + 10,
                b'A'..=b'F' if bits_per_digit == 4 => digit - b'A' + 10,
                _ => {
                    let invalid = digits[index * digits_per_byte + position..]
                        .chars()
                        .next()
                        .expect("a digit byte exists");
                    let name = if bits_per_digit == 4 {
                        "hexadecimal"
                    } else {
                        "binary"
                    };
                    return Err(error(
                        "22P02",
                        format!("\"{invalid}\" is not a valid {name} digit"),
                    ));
                }
            };
            packed |= value << (8 - bits_per_digit * (position + 1));
        }
        bytes.push_copy(packed)?;
    }
    finish_bits(bytes, length, control)
}

/// Copy network-order packed input, discarding unused low padding bits as `PostgreSQL`'s binary input does. The span contains exactly the bytes needed for the logical length.
pub fn from_packed_with_control(
    input: &[u8],
    length: u32,
    control: &ProductionControl<'_>,
) -> Result<Produced<BitStringValue>> {
    control.check()?;
    if length > MAX_BIT_LENGTH || input.len() != length.div_ceil(8) as usize {
        return Err(error(
            "22P03",
            "invalid length in external bit string".into(),
        ));
    }
    let mut bytes = ProductionVec::new(*control);
    bytes.reserve(input.len())?;
    bytes.extend_from_slice(input)?;
    if let Some(last) = bytes
        .as_mut_slice()
        .last_mut()
        .filter(|_| !length.is_multiple_of(8))
    {
        *last &= u8::MAX << (8 - length % 8);
    }
    finish_bits(bytes, length, control)
}

/// Apply assignment or explicit-cast length semantics without changing the borrowed source or allocating an unpacked temporary.
pub fn coerce_with_control(
    value: &BitStringValue,
    kind: BitStringKind,
    modifier: i32,
    explicit: bool,
    control: &ProductionControl<'_>,
) -> Result<Produced<BitStringValue>> {
    control.check()?;
    if modifier <= 0 || (kind == BitStringKind::Fixed && modifier as u32 > MAX_BIT_LENGTH) {
        return Ok(value.clone_with_control(control)?);
    }
    if !explicit {
        kind.check_length(value.bit_len(), modifier)?;
    }
    let length = match kind {
        BitStringKind::Fixed => modifier as u32,
        BitStringKind::Varying => value.bit_len().min(modifier as u32),
    };
    if length == value.bit_len() {
        return Ok(value.clone_with_control(control)?);
    }
    let byte_length = length.div_ceil(8) as usize;
    let mut bytes = ProductionVec::new(*control);
    bytes.reserve(byte_length)?;
    for position in 0..byte_length {
        let mut byte = value.as_bytes().get(position).copied().unwrap_or(0);
        if position + 1 == byte_length && !length.is_multiple_of(8) {
            byte &= u8::MAX << (8 - length % 8);
        }
        bytes.push_copy(byte)?;
    }
    finish_bits(bytes, length, control)
}

/// Emit exactly the logical bits, preserving leading zeroes and partial final bytes under the invoking output allowance.
pub fn output_with_control(
    value: &BitStringValue,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>> {
    control.check()?;
    let mut output = ProductionString::new(*control);
    output.reserve(value.bit_len() as usize)?;
    for (position, byte) in value.as_bytes().iter().copied().enumerate() {
        control.check()?;
        let count = (value.bit_len() as usize - position * 8).min(8);
        for bit in 0..count {
            output.push(if byte & (0x80 >> bit) == 0 { '0' } else { '1' })?;
        }
    }
    Ok(output.finish()?)
}

fn finish_bits(
    bytes: ProductionVec<'_, u8>,
    length: u32,
    control: &ProductionControl<'_>,
) -> Result<Produced<BitStringValue>> {
    BitStringValue::try_from_packed_with_control(bytes.finish()?, length, control)?
        .ok_or_else(|| SQLError::Internal("invalid constructed bit-string padding".into()))
}

fn error(sqlstate: &str, message: String) -> SQLError {
    SQLError::Routine {
        sqlstate: sqlstate.into(),
        message,
    }
}

#[cfg(test)]
mod tests;
