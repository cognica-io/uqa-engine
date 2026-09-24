//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The typed BLOB format chooses Value variants explicitly; its payload buffers are reserved before decoding.

use std::collections::BTreeMap;

use uqa_core::{
    json::{decode_json_string, JsonToken},
    memory::{BudgetedString, MemoryReservation},
    DecimalValue, JsonValueDecoder,
};

use super::{
    container::{scalar, validate_buffered, Container, Kind},
    retention_error, Budgeted, BudgetedVec, JsonReadError, StorageReadControl, Value,
};

mod decimal;
mod structured;

pub(super) fn decode(
    input: &[u8],
    control: &StorageReadControl,
    depth: usize,
    buffered: bool,
) -> Result<Budgeted<Value>, JsonReadError> {
    control.cancellation().check()?;
    if buffered {
        validate_buffered(input, control, depth)?;
    }
    let mut container = Container::new(input, control, depth)?;
    let mut kind = None;
    let mut content = None;
    let mut content_buffered = buffered;
    if container.kind == Kind::Array {
        let encoded = container.next()?.ok_or(JsonReadError::InvalidJson)?;
        kind = Some(string(encoded.value, control)?);
        content = Some(container.next()?.ok_or(JsonReadError::InvalidJson)?.value);
        if container.next()?.is_some() {
            return Err(JsonReadError::InvalidJson);
        }
    } else {
        while let Some(item) = container.next()? {
            let name = string(item.key.expect("object member"), control)?;
            match name.as_str() {
                "kind" => {
                    if kind.is_some() {
                        return Err(JsonReadError::InvalidJson);
                    }
                    kind = Some(string(item.value, control)?);
                }
                "value" => {
                    if content.is_some() {
                        return Err(JsonReadError::InvalidJson);
                    }
                    content_buffered |= kind.is_none();
                    content = Some(item.value);
                }
                _ => {}
            }
        }
    }
    let kind = kind.ok_or(JsonReadError::InvalidJson)?;
    if matches!(kind.as_str(), "null" | "void") {
        if let Some(content) = content {
            if scalar(content, control)? != JsonToken::Null {
                return Err(JsonReadError::InvalidJson);
            }
        }
        return Ok(Budgeted::new(
            if kind.as_str() == "null" {
                Value::Null
            } else {
                Value::Void
            },
            control.memory().empty_reservation(),
        ));
    }
    let content = content.ok_or(JsonReadError::InvalidJson)?;
    if content_buffered {
        validate_buffered(content, control, depth - 1)?;
    }
    payload(kind.as_str(), content, control, depth - 1, content_buffered)
}

fn payload(
    kind: &str,
    content: &[u8],
    control: &StorageReadControl,
    depth: usize,
    buffered: bool,
) -> Result<Budgeted<Value>, JsonReadError> {
    let plain = match kind {
        "bool" => match scalar(content, control)? {
            JsonToken::Bool(value) => Value::Bool(value),
            _ => return Err(JsonReadError::InvalidJson),
        },
        "int" => Value::Int(integer(content, control, buffered)?),
        "float_bits" => Value::Float(f64::from_bits(unsigned(content, control, buffered)?)),
        "str" | "fixed_char" | "json" | "json_b" => {
            let (text, memory) = string(content, control)?.into_parts();
            let value = match kind {
                "str" => Value::Str(text),
                "fixed_char" => Value::FixedChar(text),
                "json" => Value::Json(text),
                _ => Value::JsonB(text),
            };
            return Ok(Budgeted::new(value, memory));
        }
        "decimal" => return decimal::decode(content, control, depth),
        "bytes" => {
            let mut container = array(content, control, depth)?;
            let mut values = BudgetedVec::new(control.memory());
            while let Some(item) = container.next()? {
                let value = u8::try_from(unsigned(item.value, control, buffered)?)
                    .map_err(|_| JsonReadError::InvalidJson)?;
                values.push(value)?;
            }
            let (values, memory) = values.into_parts();
            return Ok(Budgeted::new(Value::Bytes(values), memory));
        }
        "list" | "row" => {
            let mut container = array(content, control, depth)?;
            let mut values = BudgetedVec::new(control.memory());
            let mut memory = control.memory().empty_reservation();
            while let Some(item) = container.next()? {
                let decoded = decode(item.value, control, depth - 1, buffered)?;
                values.reserve(1)?;
                let (value, retained) = decoded.into_parts();
                memory.absorb(retained);
                values.push(value)?;
            }
            let (values, retained) = values.into_parts();
            memory.absorb(retained);
            return Ok(Budgeted::new(
                if kind == "list" {
                    Value::List(values)
                } else {
                    Value::Row(values)
                },
                memory,
            ));
        }
        "record" => return structured::record(content, control, depth, buffered),
        "map" => return structured::map(content, control, depth, buffered),
        "array" => return structured::sql_array(content, control, depth, buffered),
        "legacy_vector" => {
            let value = modern(content, control, depth - 1)?;
            return if matches!(&*value, Value::LegacyVector(_)) {
                Ok(value)
            } else {
                Err(JsonReadError::InvalidJson)
            };
        }
        "temporal" => return structured::temporal(content, control, depth),
        _ => return Err(JsonReadError::InvalidJson),
    };
    Ok(Budgeted::new(plain, control.memory().empty_reservation()))
}

fn string(input: &[u8], control: &StorageReadControl) -> Result<Budgeted<String>, JsonReadError> {
    let JsonToken::String(encoded) = scalar(input, control)? else {
        return Err(JsonReadError::InvalidJson);
    };
    decode_json_string(encoded, control.memory(), control.cancellation())
}

fn integer(
    input: &[u8],
    control: &StorageReadControl,
    buffered: bool,
) -> Result<i64, JsonReadError> {
    let JsonToken::Number(text) = scalar(input, control)? else {
        return Err(JsonReadError::InvalidJson);
    };
    // serde's concrete integer deserializer receives negative zero as a float even when arbitrary-precision Value numbers retain their lexical string.
    if text == "-0" && !buffered {
        return Err(JsonReadError::InvalidJson);
    }
    text.parse().map_err(|_| JsonReadError::InvalidJson)
}

fn unsigned(
    input: &[u8],
    control: &StorageReadControl,
    buffered: bool,
) -> Result<u64, JsonReadError> {
    let JsonToken::Number(text) = scalar(input, control)? else {
        return Err(JsonReadError::InvalidJson);
    };
    if buffered && text == "-0" {
        return Ok(0);
    }
    text.parse().map_err(|_| JsonReadError::InvalidJson)
}

fn array<'a, 'c>(
    input: &'a [u8],
    control: &'c StorageReadControl,
    depth: usize,
) -> Result<Container<'a, 'c>, JsonReadError> {
    let container = Container::new(input, control, depth)?;
    if container.kind != Kind::Array {
        return Err(JsonReadError::InvalidJson);
    }
    Ok(container)
}

fn modern(
    input: &[u8],
    control: &StorageReadControl,
    depth: usize,
) -> Result<Budgeted<Value>, JsonReadError> {
    let text = std::str::from_utf8(input).map_err(|_| JsonReadError::InvalidJson)?;
    JsonValueDecoder::new(control.memory(), control.cancellation())
        .with_depth_limit(depth)
        .value(text)
}

fn insert(
    map: &mut BTreeMap<String, Value>,
    memory: &mut MemoryReservation,
    key: Budgeted<String>,
    value: Budgeted<Value>,
) -> Result<(), JsonReadError> {
    memory.grow(size_of::<(String, Value)>())?;
    let (key, key_memory) = key.into_parts();
    let (value, value_memory) = value.into_parts();
    memory.absorb(key_memory);
    memory.absorb(value_memory);
    map.insert(key, value);
    Ok(())
}

fn literal(text: &str, control: &StorageReadControl) -> Result<Budgeted<String>, JsonReadError> {
    let mut output = BudgetedString::new(control.memory());
    output.push_str(text)?;
    let (output, memory) = output.into_parts();
    Ok(Budgeted::new(output, memory))
}

fn finish(
    value: Value,
    mut memory: MemoryReservation,
    control: &StorageReadControl,
) -> Result<Budgeted<Value>, JsonReadError> {
    let required = value
        .retained_payload_bytes(control.memory(), control.cancellation())
        .map_err(retention_error)?;
    let surplus = memory
        .bytes()
        .checked_sub(required)
        .expect("precharged payload");
    drop(memory.split(surplus));
    Ok(Budgeted::new(value, memory))
}
