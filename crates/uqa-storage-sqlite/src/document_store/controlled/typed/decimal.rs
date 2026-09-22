//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Decimal typed payloads retain the nested serialized decimal envelope.

use super::{
    retention_error, string, Budgeted, Container, DecimalValue, JsonReadError, Kind,
    StorageReadControl, Value,
};

pub(super) fn decode(
    input: &[u8],
    control: &StorageReadControl,
    depth: usize,
) -> Result<Budgeted<Value>, JsonReadError> {
    let mut container = Container::new(input, control, depth)?;
    let (mut kind, mut text) = (None, None);
    if container.kind == Kind::Array {
        kind = Some(container.next()?.ok_or(JsonReadError::InvalidJson)?.value);
        text = Some(container.next()?.ok_or(JsonReadError::InvalidJson)?.value);
        if container.next()?.is_some() {
            return Err(JsonReadError::InvalidJson);
        }
    } else {
        while let Some(item) = container.next()? {
            let name = string(item.key.expect("decimal field"), control)?;
            let slot = match name.as_str() {
                "$uqa_type" => &mut kind,
                "value" => &mut text,
                _ => continue,
            };
            if slot.replace(item.value).is_some() {
                return Err(JsonReadError::InvalidJson);
            }
        }
    }
    if string(kind.ok_or(JsonReadError::InvalidJson)?, control)?.as_str() != "decimal" {
        return Err(JsonReadError::InvalidJson);
    }
    let text = string(text.ok_or(JsonReadError::InvalidJson)?, control)?;
    let parsed = DecimalValue::parse_budgeted(&text, control.memory(), control.cancellation())
        .map_err(retention_error)?
        .ok_or(JsonReadError::InvalidJson)?;
    let (value, memory) = parsed.into_parts();
    Ok(Budgeted::new(Value::Decimal(value), memory))
}
