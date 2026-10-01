//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered primitive objects lend field names and unselected strings to projection.

use super::{
    invalid_json, legacy, read_error, text, Document, StorageBackendResult, StorageReadControl,
};
use uqa_core::{
    json::{decode_json_string, JsonReader, JsonToken},
    memory::{Budgeted, BudgetedSmallVec},
    Value,
};

pub(super) fn fields(
    input: &str,
    selected: &[&str],
    control: &StorageReadControl,
) -> StorageBackendResult<Option<Budgeted<Document>>> {
    let Some(members) = ordered_primitives(input, control)? else {
        return Ok(None);
    };
    let mut memory = control.memory().empty_reservation();
    let mut result = Document::new();
    for (name, encoded) in members.iter().copied() {
        control.check()?;
        let keep = selected.contains(&name);
        let value = match encoded.first() {
            Some(b'"') if !keep => continue,
            Some(b'"') => {
                let (value, retained) =
                    decode_json_string(encoded, control.memory(), control.cancellation())
                        .map_err(read_error)?
                        .into_parts();
                memory.absorb(retained);
                Value::Str(value)
            }
            Some(b'n') => Value::Null,
            Some(b't' | b'f') => Value::Bool(encoded[0] == b't'),
            _ => {
                // Unselected numeric values still undergo their historical range conversion in field-name order.
                let (value, retained) =
                    legacy::number(text(encoded)?, legacy::Mode::Legacy, control)?.into_parts();
                memory.absorb(retained);
                value
            }
        };
        if keep {
            memory.grow(size_of::<(String, Value)>())?;
            memory.grow(name.len())?;
            result.insert(name.to_owned(), value);
        }
    }
    control.check()?;
    Ok(Some(Budgeted::new(result, memory)))
}

/// Ordinary documents keep their members inline; wider ones charge a heap buffer.
type PrimitiveMembers<'a> = BudgetedSmallVec<[(&'a str, &'a [u8]); 16]>;

fn ordered_primitives<'a>(
    input: &'a str,
    control: &StorageReadControl,
) -> StorageBackendResult<Option<PrimitiveMembers<'a>>> {
    let mut reader =
        JsonReader::new(input, control.memory(), control.cancellation()).with_depth_limit(127);
    if !matches!(
        reader
            .next_event()
            .map_err(read_error)?
            .map(|event| event.token),
        Some(JsonToken::StartObject)
    ) {
        return Ok(None);
    }
    let mut members = PrimitiveMembers::new(control.memory());
    let mut previous = None;
    loop {
        let event = reader
            .next_event()
            .map_err(read_error)?
            .ok_or_else(|| invalid_json("missing document object field"))?;
        if event.token == JsonToken::EndObject {
            break;
        }
        let JsonToken::Key(encoded) = event.token else {
            return Err(invalid_json("expected document object field"));
        };
        if encoded.contains(&b'\\') {
            return Ok(None);
        }
        let name = text(&encoded[1..encoded.len() - 1])?;
        if previous.is_some_and(|previous| previous >= name)
            || (previous.is_none()
                && matches!(
                    name,
                    "$serde_json::private::Number" | "$serde_json::private::RawValue"
                ))
        {
            return Ok(None);
        }
        let event = reader
            .next_event()
            .map_err(read_error)?
            .ok_or_else(|| invalid_json("missing document field value"))?;
        if matches!(event.token, JsonToken::StartObject | JsonToken::StartArray) {
            return Ok(None);
        }
        members.push((name, &input.as_bytes()[event.range]))?;
        previous = Some(name);
    }
    if reader.next_event().map_err(read_error)?.is_some() {
        return Err(invalid_json("trailing legacy JSON value"));
    }
    Ok(Some(members))
}

#[cfg(test)]
mod tests;
