//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Integer conversion preserves low bits, sign extension and exact-width signed output.

use super::{
    error, finish_bits, BitStringValue, Produced, ProductionControl, ProductionVec, Result,
    MAX_BIT_LENGTH,
};

/// Convert an int4 or int8 value already checked by the caller's integer-type owner. A widened int4 has the same sign-extension and low-bit behavior as int8.
pub fn from_integer_with_control(
    value: i64,
    modifier: i32,
    control: &ProductionControl<'_>,
) -> Result<Produced<BitStringValue>> {
    control.check()?;
    let length = if modifier <= 0 || modifier as u32 > MAX_BIT_LENGTH {
        1
    } else {
        modifier as u32
    };
    let mut bytes = ProductionVec::new(*control);
    bytes.reserve(length.div_ceil(8) as usize)?;
    for position in 0..length.div_ceil(8) {
        let width = (length - position * 8).min(8);
        let shift = length - position * 8 - width;
        let chunk = if shift >= 64 {
            if value < 0 {
                u8::MAX
            } else {
                0
            }
        } else {
            (value >> shift) as u8
        };
        bytes.push_copy(chunk << (8 - width))?;
    }
    finish_bits(bytes, length, control)
}

/// Interpret at most 32 bits as int4. Shorter inputs are unsigned; the full width preserves its sign bit.
pub fn to_integer(value: &BitStringValue) -> Result<i32> {
    if value.bit_len() > 32 {
        return Err(error("22003", "integer out of range".into()));
    }
    Ok(to_unsigned(value) as i32)
}

/// Interpret at most 64 bits as int8. The fixed upper bound requires no traversal scratch or retained allocation.
pub fn to_bigint(value: &BitStringValue) -> Result<i64> {
    if value.bit_len() > 64 {
        return Err(error("22003", "bigint out of range".into()));
    }
    Ok(to_unsigned(value) as i64)
}

fn to_unsigned(value: &BitStringValue) -> u64 {
    let mut output = 0_u64;
    for byte in value.as_bytes() {
        output = (output << 8) | u64::from(*byte);
    }
    output >> ((8 - value.bit_len() % 8) % 8)
}
