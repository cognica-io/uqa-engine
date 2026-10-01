//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Flat documents validate once, retain the last duplicate key, and transfer decoded names.

use super::{
    deduplicate, invalid_json, number, read_error, text, Budgeted, Document, Member, Mode,
    StorageBackendResult, StorageReadControl, Value,
};
use uqa_core::{
    json::{decode_json_string, JsonReader, JsonToken},
    memory::BudgetedVec,
};

pub(super) fn fields(
    input: &str,
    control: &StorageReadControl,
) -> StorageBackendResult<Option<Budgeted<Document>>> {
    let Some(mut members) = primitive_members(input, control)? else {
        return Ok(None);
    };
    deduplicate(&mut members, control)?;
    let (members, _members_memory) = members.into_parts();
    let mut memory = control.memory().empty_reservation();
    let mut fields = Document::new();
    for member in members {
        control.check()?;
        let value = match member.value.first() {
            Some(b'n') => Budgeted::new(Value::Null, control.memory().empty_reservation()),
            Some(b't' | b'f') => Budgeted::new(
                Value::Bool(member.value[0] == b't'),
                control.memory().empty_reservation(),
            ),
            Some(b'"') => {
                let (value, retained) =
                    decode_json_string(member.value, control.memory(), control.cancellation())
                        .map_err(read_error)?
                        .into_parts();
                Budgeted::new(Value::Str(value), retained)
            }
            _ => number(text(member.value)?, Mode::Legacy, control)?,
        };
        memory.grow(size_of::<(String, Value)>())?;
        let (name, name_memory) = member.name.into_parts();
        let (value, value_memory) = value.into_parts();
        memory.absorb(name_memory);
        memory.absorb(value_memory);
        fields.insert(name, value);
    }
    control.check()?;
    Ok(Some(Budgeted::new(fields, memory)))
}

fn primitive_members<'a>(
    input: &'a str,
    control: &StorageReadControl,
) -> StorageBackendResult<Option<BudgetedVec<Member<'a>>>> {
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
    let mut members = BudgetedVec::new(control.memory());
    loop {
        let event = reader
            .next_event()
            .map_err(read_error)?
            .ok_or_else(|| invalid_json("missing document object field"))?;
        if event.token == JsonToken::EndObject {
            break;
        }
        let JsonToken::Key(encoded_name) = event.token else {
            return Err(invalid_json("expected document object field"));
        };
        let name = decode_json_string(encoded_name, control.memory(), control.cancellation())
            .map_err(read_error)?;
        // Serde's private envelopes and nested values require the historical normalization pass, including validation of discarded duplicate subtrees. Stop immediately at a nested container without scanning its payload twice.
        if members.is_empty()
            && matches!(
                name.as_str(),
                "$serde_json::private::Number" | "$serde_json::private::RawValue"
            )
        {
            return Ok(None);
        }
        let position = event.range.start;
        let event = reader
            .next_event()
            .map_err(read_error)?
            .ok_or_else(|| invalid_json("missing document field value"))?;
        if matches!(event.token, JsonToken::StartObject | JsonToken::StartArray) {
            return Ok(None);
        }
        members.push(Member {
            name,
            encoded_name,
            value: &input.as_bytes()[event.range],
            position,
        })?;
    }
    if reader.next_event().map_err(read_error)?.is_some() {
        return Err(invalid_json("trailing legacy JSON value"));
    }
    Ok(Some(members))
}

#[cfg(test)]
mod tests;
