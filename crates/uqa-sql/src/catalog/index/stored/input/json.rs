//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrow stored index fields using Core's iterative admitted JSON grammar.

use super::{invalid, ProductionControl, SQLError};
use uqa_core::json::{
    decode_json_string_with_control, JsonEvent, JsonReadError, JsonReader, JsonToken,
};

pub(super) struct FieldSet {
    names: &'static [&'static str],
    seen: u16,
}

impl FieldSet {
    pub(super) const fn new(names: &'static [&'static str]) -> Self {
        Self { names, seen: 0 }
    }

    pub(super) fn field(&mut self, name: &str) -> Result<usize, SQLError> {
        let position = self
            .names
            .iter()
            .position(|field| *field == name)
            .ok_or_else(invalid)?;
        let bit = 1 << position;
        if self.seen & bit != 0 {
            return Err(invalid());
        }
        self.seen |= bit;
        Ok(position)
    }

    pub(super) fn require(&self, bits: u16) -> Result<(), SQLError> {
        if self.seen & bits == bits {
            Ok(())
        } else {
            Err(invalid())
        }
    }
}

pub(super) fn fields<'a>(
    text: &'a str,
    control: &ProductionControl<'_>,
    mut visit: impl FnMut(&str, &'a str) -> Result<(), SQLError>,
) -> Result<(), SQLError> {
    let mut reader = JsonReader::with_control(text, control);
    if next(&mut reader)?.token != JsonToken::StartObject {
        return Err(invalid());
    }
    loop {
        let event = next(&mut reader)?;
        match event.token {
            JsonToken::EndObject => break,
            JsonToken::Key(key) => {
                let key = decode_json_string_with_control(key, control).map_err(read_error)?;
                let value = next(&mut reader)?;
                visit(&key, value_text(text, &mut reader, value)?)?;
            }
            _ => return Err(invalid()),
        }
    }
    if reader.next_event().map_err(read_error)?.is_some() {
        return Err(invalid());
    }
    Ok(())
}

pub(super) fn array<'a>(
    text: &'a str,
    control: &ProductionControl<'_>,
    mut visit: impl FnMut(&'a str) -> Result<(), SQLError>,
) -> Result<(), SQLError> {
    let mut reader = JsonReader::with_control(text, control);
    if next(&mut reader)?.token != JsonToken::StartArray {
        return Err(invalid());
    }
    loop {
        let event = next(&mut reader)?;
        if event.token == JsonToken::EndArray {
            break;
        }
        visit(value_text(text, &mut reader, event)?)?;
    }
    if reader.next_event().map_err(read_error)?.is_some() {
        return Err(invalid());
    }
    Ok(())
}

fn next<'a>(reader: &mut JsonReader<'a, '_>) -> Result<JsonEvent<'a>, SQLError> {
    reader.next_event().map_err(read_error)?.ok_or_else(invalid)
}

fn value_text<'a>(
    text: &'a str,
    reader: &mut JsonReader<'a, '_>,
    first: JsonEvent<'a>,
) -> Result<&'a str, SQLError> {
    let mut end = first.range.end;
    let mut depth = usize::from(matches!(
        first.token,
        JsonToken::StartArray | JsonToken::StartObject
    ));
    while depth != 0 {
        let event = next(reader)?;
        match event.token {
            JsonToken::StartArray | JsonToken::StartObject => depth += 1,
            JsonToken::EndArray | JsonToken::EndObject => depth -= 1,
            _ => {}
        }
        end = event.range.end;
    }
    Ok(&text[first.range.start..end])
}

pub(super) fn object_id(text: &str, control: &ProductionControl<'_>) -> Result<[u8; 16], SQLError> {
    let mut result = [0; 16];
    let mut position = 0;
    array(text, control, |value| {
        *result.get_mut(position).ok_or_else(invalid)? = value.parse().map_err(|_| invalid())?;
        position += 1;
        Ok(())
    })?;
    if position != result.len() {
        return Err(invalid());
    }
    Ok(result)
}

pub(super) fn boolean(text: &str) -> Result<bool, SQLError> {
    match text {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(invalid()),
    }
}

pub(super) fn require_array(text: &str) -> Result<(), SQLError> {
    if text.starts_with('[') {
        Ok(())
    } else {
        Err(invalid())
    }
}

pub(super) fn read_error(error: JsonReadError) -> SQLError {
    match error {
        JsonReadError::Memory(error) => error.into(),
        JsonReadError::Cancelled(error) => error.into(),
        JsonReadError::InvalidJson | JsonReadError::Malformed { .. } => invalid(),
    }
}
