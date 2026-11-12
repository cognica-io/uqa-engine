//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog envelopes retain original decoded fields without a temporary JSON tree.

use crate::{read_control::StorageReadControl, StorageBackendError, StorageBackendResult};
use serde::de::Error as _;
use uqa_core::{
    json::{decode_json_string, JsonReadError, JsonReader, JsonToken},
    memory::{Budgeted, MemoryReservation},
};

/// Decode the stored struct's map or positional representation. Unknown map fields remain ignored, duplicate known fields fail, and nullable fields retain their absent/null default.
pub(crate) fn fields<const N: usize>(
    bytes: &[u8],
    names: [(&'static str, bool); N],
    control: &StorageReadControl,
) -> StorageBackendResult<[Option<Budgeted<String>>; N]> {
    fields_with(bytes, names, control, |index, value| {
        string_field(value, names[index], control)
    })
}

/// Decode typed fields through the same map/sequence grammar and ignored-field policy.
pub(crate) fn fields_with<'a, T: Default, const N: usize>(
    bytes: &'a [u8],
    names: [(&'static str, bool); N],
    control: &StorageReadControl,
    decode: impl FnMut(usize, JsonToken<'a>) -> StorageBackendResult<T>,
) -> StorageBackendResult<[T; N]> {
    control.check()?;
    let mut reader = JsonReader::from_slice(bytes, control.memory(), control.cancellation())
        .with_ignored_string_escapes();
    let (result, _) = read_fields(&mut reader, names, control, decode)?;
    if reader.next_event().map_err(json_error)?.is_some() {
        return Err(invalid("trailing catalog value"));
    }
    control.check()?;
    Ok(result)
}

/// Read one original struct and return its closing delimiter offset, leaving the surrounding metadata suffix untouched.
pub(crate) fn fields_prefix<const N: usize>(
    bytes: &[u8],
    names: [(&'static str, bool); N],
    control: &StorageReadControl,
) -> StorageBackendResult<([Option<Budgeted<String>>; N], usize)> {
    control.check()?;
    let mut reader = JsonReader::from_slice(bytes, control.memory(), control.cancellation())
        .with_ignored_string_escapes();
    let result = read_fields(&mut reader, names, control, |index, value| {
        string_field(value, names[index], control)
    })?;
    control.check()?;
    Ok(result)
}

fn read_fields<'a, T: Default, const N: usize>(
    reader: &mut JsonReader<'a, '_>,
    names: [(&'static str, bool); N],
    control: &StorageReadControl,
    mut decode: impl FnMut(usize, JsonToken<'a>) -> StorageBackendResult<T>,
) -> StorageBackendResult<([T; N], usize)> {
    let first = next(reader)?;
    let object = match first {
        JsonToken::StartObject => true,
        JsonToken::StartArray => false,
        _ => return Err(invalid("expected a catalog struct")),
    };
    let mut seen = [false; N];
    let mut result = std::array::from_fn(|_| T::default());
    let mut position = 0;
    let end = loop {
        let event = reader
            .next_event()
            .map_err(json_error)?
            .ok_or_else(|| invalid("unexpected end of catalog text"))?;
        let token = event.token;
        if matches!(
            (object, token),
            (true, JsonToken::EndObject) | (false, JsonToken::EndArray)
        ) {
            break event.range.end;
        }
        let selected = if object {
            let JsonToken::Key(key) = token else {
                return Err(invalid("expected a catalog field name"));
            };
            let key = decode_json_string(key, control.memory(), control.cancellation())
                .map_err(json_error)?;
            names.iter().position(|(name, _)| *name == key.as_str())
        } else {
            if position == N {
                return Err(invalid("too many positional catalog fields"));
            }
            let selected = position;
            position += 1;
            Some(selected)
        };
        if let Some(index) = selected.filter(|index| seen[*index]) {
            return Err(invalid(format!("duplicate field `{}`", names[index].0)));
        }
        let value = if object { next(reader)? } else { token };
        let Some(index) = selected else {
            skip(value, reader)?;
            continue;
        };
        seen[index] = true;
        result[index] = decode(index, value)?;
    };
    for (index, (name, nullable)) in names.into_iter().enumerate() {
        if !seen[index] && !nullable {
            return Err(invalid(format!("missing field `{name}`")));
        }
    }
    Ok((result, end))
}

fn string_field(
    value: JsonToken<'_>,
    (name, nullable): (&str, bool),
    control: &StorageReadControl,
) -> StorageBackendResult<Option<Budgeted<String>>> {
    match value {
        JsonToken::String(value) => Ok(Some(
            decode_json_string(value, control.memory(), control.cancellation())
                .map_err(json_error)?,
        )),
        JsonToken::Null if nullable => Ok(None),
        _ => Err(invalid(format!("invalid string field `{name}`"))),
    }
}

pub(super) fn next<'a>(reader: &mut JsonReader<'a, '_>) -> StorageBackendResult<JsonToken<'a>> {
    reader
        .next_event()
        .map_err(json_error)?
        .map(|event| event.token)
        .ok_or_else(|| invalid("unexpected end of catalog text"))
}

fn skip(first: JsonToken<'_>, reader: &mut JsonReader<'_, '_>) -> StorageBackendResult<()> {
    let mut depth = usize::from(matches!(
        first,
        JsonToken::StartArray | JsonToken::StartObject
    ));
    while depth != 0 {
        match next(reader)? {
            JsonToken::StartArray | JsonToken::StartObject => depth += 1,
            JsonToken::EndArray | JsonToken::EndObject => depth -= 1,
            _ => (),
        }
    }
    Ok(())
}

pub(crate) fn invalid(message: impl std::fmt::Display) -> StorageBackendError {
    StorageBackendError::Serde(serde_json::Error::custom(message))
}

pub(crate) fn json_error(error: JsonReadError) -> StorageBackendError {
    match error {
        JsonReadError::Memory(error) => error.into(),
        JsonReadError::Cancelled(error) => error.into(),
        error => invalid(error),
    }
}

pub(crate) fn take<T>(value: Budgeted<T>, memory: &mut MemoryReservation) -> T {
    let (value, payload) = value.into_parts();
    memory.absorb(payload);
    value
}

/// Flattened serde content validates every retained or overwritten value before field projection. Arbitrary-precision numbers keep their original lexical representation; strict strings and container depth are checked without constructing a discarded numeric owner.
pub(crate) fn validate_content(
    bytes: &[u8],
    remaining_depth: usize,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    let mut reader = JsonReader::from_slice(bytes, control.memory(), control.cancellation())
        .with_depth_limit(remaining_depth);
    while reader.next_event().map_err(json_error)?.is_some() {}
    Ok(())
}
