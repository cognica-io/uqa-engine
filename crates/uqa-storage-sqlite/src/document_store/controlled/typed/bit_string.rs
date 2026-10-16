//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Packed bit payloads keep the typed codec's field rules and original read allowance.

use super::{
    array, string, unsigned, Budgeted, BudgetedVec, Container, JsonReadError, Kind,
    StorageReadControl, Value,
};

pub(super) fn decode(
    input: &[u8],
    control: &StorageReadControl,
    depth: usize,
    buffered: bool,
) -> Result<Budgeted<Value>, JsonReadError> {
    let mut container = Container::new(input, control, depth)?;
    let (mut bytes, mut length) = (None, None);
    if container.kind == Kind::Array {
        bytes = Some(container.next()?.ok_or(JsonReadError::InvalidJson)?.value);
        length = Some(container.next()?.ok_or(JsonReadError::InvalidJson)?.value);
        if container.next()?.is_some() {
            return Err(JsonReadError::InvalidJson);
        }
    } else {
        while let Some(item) = container.next()? {
            let name = string(item.key.expect("packed bit field"), control)?;
            let slot = match name.as_str() {
                "bytes" => &mut bytes,
                "bit_len" => &mut length,
                _ => continue,
            };
            if slot.replace(item.value).is_some() {
                return Err(JsonReadError::InvalidJson);
            }
        }
    }
    let length = u32::try_from(unsigned(
        length.ok_or(JsonReadError::InvalidJson)?,
        control,
        buffered,
    )?)
    .map_err(|_| JsonReadError::InvalidJson)?;
    let mut bytes = array(bytes.ok_or(JsonReadError::InvalidJson)?, control, depth - 1)?;
    let mut packed = BudgetedVec::new(control.memory());
    while let Some(item) = bytes.next()? {
        let byte = u8::try_from(unsigned(item.value, control, buffered)?)
            .map_err(|_| JsonReadError::InvalidJson)?;
        packed.push(byte)?;
    }
    let (packed, mut memory) = packed.into_parts();
    control.cancellation().check()?;
    memory.grow(uqa_core::BitStringValue::retained_header_bytes())?;
    let bits = uqa_core::BitStringValue::try_from_packed(packed, length)
        .ok_or(JsonReadError::InvalidJson)?;
    Ok(Budgeted::new(Value::BitString(bits), memory))
}
