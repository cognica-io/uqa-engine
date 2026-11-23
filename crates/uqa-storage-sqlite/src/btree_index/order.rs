//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Keys of the physical lookup index whose byte order is the order of their values. One seek finds a value and one interval holds a range.

use super::{Result, Value};
use uqa_core::memory::BudgetedVec;
use uqa_storage::read_control::StorageReadControl;

#[cfg(test)]
mod tests;

/// The kinds of the domain, in the order `Value` gives them.
const NULL: u8 = 0;
const NUMBER: u8 = 2;
const TEXT: u8 = 3;
const BYTES: u8 = 5;
const ROW: u8 = 11;
/// The first key of a value that is no scalar: every row and every bit string follows it, and every scalar precedes it.
pub(super) const AFTER_SCALARS: [u8; 1] = [ROW];
const BITS: u8 = 17;
/// Follows the last field of a row, before every kind, so that a row sorts after its own beginnings.
const ROW_END: u8 = 1;
/// A null field of a row, after every kind: a row orders its null fields last.
const NULL_FIELD: u8 = 255;

/// The sign of a number, then its magnitude for the numbers of one sign.
const NEGATIVE: u8 = 0;
const ZERO: u8 = 1;
const POSITIVE: u8 = 2;
/// Added to the binary exponent of a magnitude, the least of which is 2^-1074.
const EXPONENT_BIAS: i32 = 1075;
const INFINITE_EXPONENT: u16 = u16::MAX;

/// A key that is equal for two values exactly when `Value` compares them equal, and whose bytes order as `Value` orders the values: for the infallible scalar domain and the rows composed of it. `None` for a value outside that domain, which an index stores without a key and so declines.
pub(super) fn key(value: &Value, control: &StorageReadControl) -> Result<Option<BudgetedVec<u8>>> {
    let mut output = BudgetedVec::new(control.memory());
    // The fields still to be written, last first, and after the fields of a row the end of that row.
    let mut pending: BudgetedVec<Option<(&Value, bool)>> = BudgetedVec::new(control.memory());
    pending.push(Some((value, false)))?;
    while let Some(next) = pending.pop() {
        control.check()?;
        let Some((value, field)) = next else {
            output.push(ROW_END)?;
            continue;
        };
        match value {
            Value::Null => output.push(if field { NULL_FIELD } else { NULL })?,
            Value::Bool(value) => integer(i64::from(*value), &mut output)?,
            Value::Int(value) => integer(*value, &mut output)?,
            Value::Float(value) => {
                if value.is_nan() {
                    return Ok(None);
                }
                float(*value, &mut output)?;
            }
            Value::Str(value) => terminated(TEXT, value.as_bytes(), &mut output)?,
            Value::Bytes(value) => terminated(BYTES, value, &mut output)?,
            Value::BitString(value) => {
                terminated(BITS, value.as_bytes(), &mut output)?;
                output.extend_from_slice(&value.bit_len().to_be_bytes())?;
            }
            Value::Row(values) => {
                output.push(ROW)?;
                pending.push(None)?;
                for child in values.iter().rev() {
                    pending.push(Some((child, true)))?;
                }
            }
            _ => return Ok(None),
        }
    }
    Ok(Some(output))
}

fn integer(value: i64, output: &mut BudgetedVec<u8>) -> Result<()> {
    let magnitude = value.unsigned_abs();
    if magnitude == 0 {
        return number(ZERO, 0, 0, output);
    }
    let shift = magnitude.leading_zeros();
    let exponent = biased(63 - shift.cast_signed());
    number(sign(value < 0), exponent, magnitude << shift, output)
}

fn float(value: f64, output: &mut BudgetedVec<u8>) -> Result<()> {
    let bits = value.abs().to_bits();
    let exponent = (bits >> 52) & 0x7ff;
    let fraction = bits & ((1 << 52) - 1);
    let negative = value.is_sign_negative();
    match (exponent, fraction) {
        (0, 0) => number(ZERO, 0, 0, output),
        // A subnormal magnitude is its fraction in units of 2^-1074.
        (0, fraction) => {
            let shift = fraction.leading_zeros();
            number(
                sign(negative),
                biased(63 - shift.cast_signed() - 1074),
                fraction << shift,
                output,
            )
        }
        (0x7ff, _) => number(sign(negative), INFINITE_EXPONENT, 0, output),
        (exponent, fraction) => number(
            sign(negative),
            biased(i32::try_from(exponent).expect("eleven bits") - 1023),
            (1 << 63) | (fraction << 11),
            output,
        ),
    }
}

fn sign(negative: bool) -> u8 {
    if negative {
        NEGATIVE
    } else {
        POSITIVE
    }
}

fn biased(exponent: i32) -> u16 {
    u16::try_from(exponent + EXPONENT_BIAS).expect("a binary exponent of a number")
}

/// A magnitude is its leading bit at `exponent` and the bits after it. The greater magnitude is the lesser of two negative numbers, so theirs is written inverted.
fn number(sign: u8, exponent: u16, mantissa: u64, output: &mut BudgetedVec<u8>) -> Result<()> {
    output.push(NUMBER)?;
    output.push(sign)?;
    match sign {
        ZERO => {}
        NEGATIVE => {
            output.extend_from_slice(&(!exponent).to_be_bytes())?;
            output.extend_from_slice(&(!mantissa).to_be_bytes())?;
        }
        _ => {
            output.extend_from_slice(&exponent.to_be_bytes())?;
            output.extend_from_slice(&mantissa.to_be_bytes())?;
        }
    }
    Ok(())
}

/// Bytes end with two zero bytes, and a zero byte among them is followed by 255: a value sorts before every value it begins.
fn terminated(kind: u8, value: &[u8], output: &mut BudgetedVec<u8>) -> Result<()> {
    output.push(kind)?;
    for &byte in value {
        output.push(byte)?;
        if byte == 0 {
            output.push(255)?;
        }
    }
    output.extend_from_slice(&[0, 0])?;
    Ok(())
}
