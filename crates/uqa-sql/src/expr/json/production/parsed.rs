//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{normalize_fields, Field, Node, Values};
use crate::error::Result;
use uqa_core::json::{decode_json_string_with_control, JsonReadError, JsonReader, JsonToken};
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionString},
    ValueRetentionError,
};

pub(super) fn parse(text: &str, control: &ProductionControl<'_>) -> Result<Node> {
    let mut reader = JsonReader::with_control(text, control).with_depth_limit(127);
    parse_node(&mut reader, control).map_err(|error| match error {
        JsonReadError::InvalidJson | JsonReadError::Malformed { .. } => {
            super::super::diagnostics::invalid_input(text, reader.syntax_position())
        }
        JsonReadError::Memory(error) => error.into(),
        JsonReadError::Cancelled(error) => error.into(),
    })
}

pub(super) fn parse_optional(text: &str, control: &ProductionControl<'_>) -> Result<Option<Node>> {
    match parse(text, control) {
        Ok(node) => Ok(Some(node)),
        Err(error) if error.sqlstate() == Some("22P02") => Ok(None),
        Err(error) => Err(error),
    }
}

fn parse_node(
    reader: &mut JsonReader<'_, '_>,
    control: &ProductionControl<'_>,
) -> std::result::Result<Node, JsonReadError> {
    let token = reader
        .next_event()?
        .ok_or(JsonReadError::InvalidJson)?
        .token;
    let node = consume(reader, token, control)?;
    if reader.next_event()?.is_some() {
        return Err(JsonReadError::InvalidJson);
    }
    Ok(node)
}

fn consume(
    reader: &mut JsonReader<'_, '_>,
    token: JsonToken<'_>,
    control: &ProductionControl<'_>,
) -> std::result::Result<Node, JsonReadError> {
    control.check()?;
    Ok(match token {
        JsonToken::Null => Node::Null,
        JsonToken::Bool(value) => Node::Bool(value),
        JsonToken::Number(text) => Node::Number(number_text(text, control)?),
        JsonToken::String(text) => Node::String(decode_json_string_with_control(text, control)?),
        JsonToken::StartArray => {
            let mut values = Values::new(control);
            loop {
                let token = reader
                    .next_event()?
                    .ok_or(JsonReadError::InvalidJson)?
                    .token;
                if token == JsonToken::EndArray {
                    break;
                }
                values.push(consume(reader, token, control)?, control)?;
            }
            Node::Array(values)
        }
        JsonToken::StartObject => object(reader, control)?,
        _ => return Err(JsonReadError::InvalidJson),
    })
}

fn object(
    reader: &mut JsonReader<'_, '_>,
    control: &ProductionControl<'_>,
) -> std::result::Result<Node, JsonReadError> {
    let mut values = Values::new(control);
    loop {
        let token = reader
            .next_event()?
            .ok_or(JsonReadError::InvalidJson)?
            .token;
        if token == JsonToken::EndObject {
            break;
        }
        let JsonToken::Key(key) = token else {
            return Err(JsonReadError::InvalidJson);
        };
        let key = decode_json_string_with_control(key, control)?;
        let token = reader
            .next_event()?
            .ok_or(JsonReadError::InvalidJson)?
            .token;
        // Preserve serde_json's arbitrary-precision private-number interpretation at the first object key.
        if values.is_empty() && key.as_str() == "$serde_json::private::Number" {
            let JsonToken::String(text) = token else {
                return Err(JsonReadError::InvalidJson);
            };
            let text = decode_json_string_with_control(text, control)?;
            let mut number_reader = JsonReader::with_control(&text, control);
            let event = number_reader
                .next_event()?
                .ok_or(JsonReadError::InvalidJson)?;
            let JsonToken::Number(number) = event.token else {
                return Err(JsonReadError::InvalidJson);
            };
            if event.range.start != 0
                || event.range.end != text.len()
                || number_reader.next_event()?.is_some()
            {
                return Err(JsonReadError::InvalidJson);
            }
            let value = Node::Number(number_text(number, control)?);
            if reader
                .next_event()?
                .ok_or(JsonReadError::InvalidJson)?
                .token
                != JsonToken::EndObject
            {
                return Err(JsonReadError::InvalidJson);
            }
            return Ok(value);
        }
        let value = consume(reader, token, control)?;
        values.push(
            Field {
                key,
                value,
                ordinal: values.len(),
            },
            control,
        )?;
    }
    normalize_fields(&mut values, control)?;
    Ok(Node::Object(values))
}

/// Match `serde_json`'s arbitrary-precision number carrier: signed integer zero normalizes, and exponents use lower-case e with an explicit sign. Fractional digits and exponent zero padding stay lexical.
fn number_text(
    text: &str,
    control: &ProductionControl<'_>,
) -> std::result::Result<Produced<String>, ValueRetentionError> {
    control.check()?;
    if text == "-0" {
        return control.copy_text("0");
    }
    let mut output = ProductionString::new(*control);
    let mut exponent = None;
    for (index, byte) in text.bytes().enumerate() {
        if index.is_multiple_of(4096) {
            control.check()?;
        }
        if matches!(byte, b'e' | b'E') {
            exponent = Some(index);
            break;
        }
    }
    if let Some(index) = exponent {
        output.push_str(&text[..index])?;
        output.push('e')?;
        let suffix = &text[index + 1..];
        if !suffix.starts_with(['+', '-']) {
            output.push('+')?;
        }
        output.push_str(suffix)?;
    } else {
        output.push_str(text)?;
    }
    output.finish()
}
