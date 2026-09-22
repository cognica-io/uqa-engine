//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Unversioned documents retain the historical JSON normalization, tagged-value conversion and byte-array preference in that order.

use uqa_core::{
    json::{decode_json_string, JsonReader, JsonToken},
    memory::{Budgeted, BudgetedVec},
    Value,
};

use super::{
    decoder, invalid_json, other_error, read_error,
    spans::{ContainerSpans, Member},
    text, Document, StorageBackendResult, StorageReadControl,
};

mod normalized;
use normalized::{private_text, PrivateKind};

#[derive(Clone, Copy)]
enum Mode {
    Legacy,
    Modern,
}

pub(super) fn fields(
    input: &str,
    control: &StorageReadControl,
) -> StorageBackendResult<Budgeted<Document>> {
    // The old intermediate serde_json::Value validates every original member before duplicate keys disappear, including malformed private Number/RawValue objects.
    normalized::validate(input, control)?;
    document(input, control)
}

pub(super) fn single_value(
    input: &str,
    control: &StorageReadControl,
) -> StorageBackendResult<Budgeted<Value>> {
    normalized::validate(input, control)?;
    value(input, Mode::Legacy, control)
}

fn document(input: &str, control: &StorageReadControl) -> StorageBackendResult<Budgeted<Document>> {
    let mut reader = JsonReader::new(input, control.memory(), control.cancellation());
    if !matches!(
        reader
            .next_event()
            .map_err(read_error)?
            .map(|event| event.token),
        Some(JsonToken::StartObject)
    ) {
        return Err(other_error(
            "persisted KeyValue document is not a JSON object",
        ));
    }
    drop(reader);
    let ContainerSpans::Object(mut members) = ContainerSpans::read(input.as_bytes(), control)?
    else {
        return Err(other_error(
            "persisted KeyValue document is not a JSON object",
        ));
    };
    if let Some((kind, text)) = private_text(&members, control)? {
        return match kind {
            PrivateKind::Raw => document(&text, control),
            PrivateKind::Number => Err(other_error(
                "persisted KeyValue document is not a JSON object",
            )),
        };
    }
    deduplicate(&mut members, control)?;
    object_fields(&members, Mode::Legacy, control)
}

fn value(
    input: &str,
    mode: Mode,
    control: &StorageReadControl,
) -> StorageBackendResult<Budgeted<Value>> {
    control.check()?;
    let mut reader = JsonReader::new(input, control.memory(), control.cancellation());
    let event = reader
        .next_event()
        .map_err(read_error)?
        .ok_or_else(|| invalid_json("missing legacy document value"))?;
    match event.token {
        JsonToken::StartObject => object(input, mode, control),
        JsonToken::StartArray => array(input, mode, control),
        JsonToken::Number(text) => number(text, mode, control),
        _ => decoder(control).value(input).map_err(read_error),
    }
}

fn object(
    input: &str,
    mode: Mode,
    control: &StorageReadControl,
) -> StorageBackendResult<Budgeted<Value>> {
    let ContainerSpans::Object(mut members) = ContainerSpans::read(input.as_bytes(), control)?
    else {
        unreachable!("legacy object token");
    };
    if let Some((kind, text)) = private_text(&members, control)? {
        return match kind {
            PrivateKind::Raw => value(&text, mode, control),
            PrivateKind::Number => number(&text, mode, control),
        };
    }
    deduplicate(&mut members, control)?;
    if matches!(mode, Mode::Modern)
        || members
            .iter()
            .any(|member| member.name.as_str() == "$uqa_type")
    {
        let fields = object_fields(&members, Mode::Modern, control)?;
        let tagged =
            Value::from_json_fields_budgeted(fields, control.cancellation()).map_err(read_error)?;
        if matches!(mode, Mode::Modern) || !matches!(&*tagged, Value::Map(_)) {
            return Ok(tagged);
        }
        // A failed tag keeps the original normalized JSON subtree's legacy array and number rules, rather than reusing already converted modern children.
        drop(tagged);
    }
    let (fields, memory) = object_fields(&members, Mode::Legacy, control)?.into_parts();
    Ok(Budgeted::new(Value::Map(fields), memory))
}

fn object_fields(
    members: &[Member<'_>],
    mode: Mode,
    control: &StorageReadControl,
) -> StorageBackendResult<Budgeted<Document>> {
    let mut memory = control.memory().empty_reservation();
    let mut fields = Document::new();
    for member in members {
        control.check()?;
        let name = decode_json_string(
            member.encoded_name,
            control.memory(),
            control.cancellation(),
        )
        .map_err(read_error)?;
        let value = value(text(member.value)?, mode, control)?;
        memory.grow(size_of::<(String, Value)>())?;
        let (name, name_memory) = name.into_parts();
        let (value, value_memory) = value.into_parts();
        memory.absorb(name_memory);
        memory.absorb(value_memory);
        fields.insert(name, value);
    }
    Ok(Budgeted::new(fields, memory))
}

fn array(
    input: &str,
    mode: Mode,
    control: &StorageReadControl,
) -> StorageBackendResult<Budgeted<Value>> {
    let ContainerSpans::Array(spans) = ContainerSpans::read(input.as_bytes(), control)? else {
        unreachable!("legacy array token");
    };
    let mut memory = control.memory().empty_reservation();
    let mut values = BudgetedVec::new(control.memory());
    for raw in spans.iter() {
        let value = value(text(raw)?, mode, control)?;
        values.reserve(1)?;
        let (value, retained) = value.into_parts();
        memory.absorb(retained);
        values.push(value)?;
    }
    if matches!(mode, Mode::Legacy)
        && values
            .iter()
            .all(|value| matches!(value, Value::Int(value) if u8::try_from(*value).is_ok()))
    {
        let mut bytes = BudgetedVec::new(control.memory());
        bytes.reserve(values.len())?;
        for value in values.iter() {
            control.check()?;
            let Value::Int(value) = value else {
                unreachable!("legacy byte array candidate");
            };
            bytes.push(u8::try_from(*value).expect("validated byte"))?;
        }
        let (bytes, retained) = bytes.into_parts();
        Ok(Budgeted::new(Value::Bytes(bytes), retained))
    } else {
        let (values, retained) = values.into_parts();
        memory.absorb(retained);
        Ok(Budgeted::new(Value::List(values), memory))
    }
}

fn number(
    text: &str,
    mode: Mode,
    control: &StorageReadControl,
) -> StorageBackendResult<Budgeted<Value>> {
    control.check()?;
    let value = if let Ok(value) = text.parse::<i64>() {
        Value::Int(value)
    } else if let Ok(value) = text.parse::<u64>() {
        Value::Float(value as f64)
    } else if matches!(mode, Mode::Modern) {
        // serde_json 1.0.149's Number deserializer tries 128-bit integer visitors before its private number map. Value's ordinary visitor rejects those integer widths; preserve that from_value boundary inside historical tags.
        if text.parse::<u128>().is_ok() || text.parse::<i128>().is_ok() {
            return Err(invalid_json(
                "legacy tagged value has an unsupported 128-bit integer",
            ));
        }
        return decoder(control).value(text).map_err(read_error);
    } else {
        let value = text
            .parse::<f64>()
            .ok()
            .filter(|value| value.is_finite())
            .ok_or_else(|| {
                other_error(
                    "persisted KeyValue document number is outside the supported numeric range",
                )
            })?;
        Value::Float(value)
    };
    control.check()?;
    Ok(Budgeted::new(value, control.memory().empty_reservation()))
}

fn deduplicate(
    members: &mut BudgetedVec<Member<'_>>,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    control.check()?;
    members.sort_unstable_by(|left, right| {
        left.name
            .as_str()
            .cmp(right.name.as_str())
            .then_with(|| right.position.cmp(&left.position))
    });
    let mut unique = 0;
    for index in 0..members.len() {
        control.check()?;
        if unique == 0 || members[unique - 1].name.as_str() != members[index].name.as_str() {
            members.swap(unique, index);
            unique += 1;
        }
    }
    members.truncate(unique);
    Ok(())
}
