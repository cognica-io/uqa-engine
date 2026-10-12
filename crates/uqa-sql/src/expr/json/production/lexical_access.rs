//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Textual JSON extraction retains the selected source bytes and borrows every unselected value.

use std::ops::Range;

use crate::{expr::conversion::value_to_string_with_control, SQLError};
use uqa_core::{
    json::{decode_json_string_with_control, JsonEvent, JsonReadError, JsonReader, JsonToken},
    memory::{Produced, ProductionControl},
    Value,
};

#[cfg(test)]
mod tests;

pub(super) fn extract<K: std::borrow::Borrow<Value>>(
    text: &str,
    keys: &[K],
    as_text: bool,
    array_index: Option<bool>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    let mut reader = JsonReader::with_control(text, control).with_depth_limit(127);
    let first = next(&mut reader, text, control)?;
    let scalar = !matches!(first.token, JsonToken::StartArray | JsonToken::StartObject);
    let mut selected = value_range(&mut reader, first, text, control)?;
    if reader
        .next_event()
        .map_err(|error| read_error(error, text))?
        .is_some()
    {
        return Err(crate::expr::json_strip::invalid_json_input(text));
    }
    // PostgreSQL's empty-path scalar callback retains the original prefix before its token.
    if scalar && keys.is_empty() {
        selected.start = 0;
    }
    for key in keys {
        let key = value_to_string_with_control(key.borrow(), control)?;
        let Some(child) = child_range(&text[selected.clone()], &key, array_index, control)? else {
            return super::inline(Value::Null, control);
        };
        selected = selected.start + child.start..selected.start + child.end;
    }
    let source = &text[selected];
    if as_text {
        let mut reader = JsonReader::with_control(source, control);
        match next(&mut reader, source, control)?.token {
            JsonToken::Null => return super::inline(Value::Null, control),
            JsonToken::String(encoded) => {
                let decoded = decode_json_string_with_control(encoded, control)
                    .map_err(|error| read_error(error, source))?;
                return super::text_value(decoded, None, control);
            }
            _ => {}
        }
    }
    super::text_value(
        control.copy_text(source)?,
        (!as_text).then_some(false),
        control,
    )
}

fn child_range(
    text: &str,
    key: &str,
    array_index: Option<bool>,
    control: &ProductionControl<'_>,
) -> Result<Option<Range<usize>>, SQLError> {
    let mut reader = JsonReader::with_control(text, control).with_depth_limit(127);
    let root = next(&mut reader, text, control)?;
    match root.token {
        JsonToken::StartObject if array_index != Some(true) => {
            let mut selected = None;
            loop {
                let event = next(&mut reader, text, control)?;
                if event.token == JsonToken::EndObject {
                    break;
                }
                let JsonToken::Key(encoded) = event.token else {
                    return Err(crate::expr::json_strip::invalid_json_input(text));
                };
                let name = decode_json_string_with_control(encoded, control)
                    .map_err(|error| read_error(error, text))?;
                let first = next(&mut reader, text, control)?;
                let range = value_range(&mut reader, first, text, control)?;
                if name.as_str() == key {
                    selected = Some(range);
                }
            }
            Ok(selected)
        }
        JsonToken::StartArray if array_index != Some(false) => {
            let Ok(index) = key.parse::<i64>() else {
                return Ok(None);
            };
            let mut count = 0_i64;
            let mut selected = None;
            loop {
                let event = next(&mut reader, text, control)?;
                if event.token == JsonToken::EndArray {
                    break;
                }
                let range = value_range(&mut reader, event, text, control)?;
                if count == index {
                    selected = Some(range);
                }
                count += 1;
            }
            if index < 0 {
                let normalized = count + index;
                if normalized >= 0 {
                    let key = control.format(format_args!("{normalized}"))?;
                    return child_range(text, &key, array_index, control);
                }
            }
            Ok(selected)
        }
        _ => Ok(None),
    }
}

fn value_range(
    reader: &mut JsonReader<'_, '_>,
    first: JsonEvent<'_>,
    text: &str,
    control: &ProductionControl<'_>,
) -> Result<Range<usize>, SQLError> {
    let mut range = first.range;
    let mut depth = usize::from(matches!(
        first.token,
        JsonToken::StartArray | JsonToken::StartObject
    ));
    while depth != 0 {
        let event = next(reader, text, control)?;
        match event.token {
            JsonToken::StartArray | JsonToken::StartObject => depth += 1,
            JsonToken::EndArray | JsonToken::EndObject => depth -= 1,
            _ => {}
        }
        range.end = event.range.end;
    }
    Ok(range)
}

fn next<'a>(
    reader: &mut JsonReader<'a, '_>,
    text: &str,
    control: &ProductionControl<'_>,
) -> Result<JsonEvent<'a>, SQLError> {
    let event = reader
        .next_event()
        .map_err(|error| read_error(error, text))?
        .ok_or_else(|| crate::expr::json_strip::invalid_json_input(text))?;
    if let JsonToken::Key(encoded) | JsonToken::String(encoded) = event.token {
        let mut offset = 0;
        let mut next_check = 0;
        while offset < encoded.len() {
            if offset >= next_check {
                control.check()?;
                next_check = offset.saturating_add(4096);
            }
            if encoded[offset] == b'\\' {
                if encoded.get(offset + 1..offset + 6) == Some(b"u0000") {
                    return Err(SQLError::Diagnostic {
                        sqlstate: "22P05".into(),
                        message: "unsupported Unicode escape sequence".into(),
                        detail: Some("\\u0000 cannot be converted to text.".into()),
                        hint: None,
                    });
                }
                offset += 1;
            }
            offset += 1;
        }
    }
    Ok(event)
}

fn read_error(error: JsonReadError, text: &str) -> SQLError {
    match error {
        JsonReadError::Memory(error) => error.into(),
        JsonReadError::Cancelled(error) => error.into(),
        JsonReadError::InvalidJson | JsonReadError::Malformed { .. } => {
            crate::expr::json_strip::invalid_json_input(text)
        }
    }
}
