//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical bit payloads retain their own length, packed bytes and read allowance.

use super::{corrupt, Produced, ProductionControl, ProductionVec, SQLError, Value};
use uqa_core::BitStringValue;

pub(super) fn read(
    bytes: &[u8],
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    control.check()?;
    let header = bytes
        .get(..4)
        .ok_or_else(|| corrupt("truncated bit-string length"))?;
    let length = u32::from_le_bytes(header.try_into().expect("four length bytes"));
    if length > i32::MAX as u32 - 7 {
        return Err(corrupt("invalid bit-string length"));
    }
    let data = bytes
        .get(4..4 + length.div_ceil(8) as usize)
        .ok_or_else(|| corrupt("truncated bit-string data"))?;
    let mut packed = ProductionVec::new(*control);
    packed.reserve(data.len())?;
    for byte in data {
        packed.push_copy(*byte)?;
    }
    let bits = BitStringValue::try_from_packed_with_control(packed.finish()?, length, control)?
        .ok_or_else(|| corrupt("invalid bit-string padding"))?;
    let (bits, memory) = bits.into_parts();
    Ok(control.finish(Value::BitString(bits), memory)?)
}
