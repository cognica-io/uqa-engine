//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! String-only catalog envelopes retain their original decoded field buffers.

use crate::{read_control::StorageReadControl, StorageBackendError, StorageBackendResult};
use serde::de::Error as _;
use uqa_core::{
    json::{decode_json_string, JsonReadError, JsonReader, JsonToken},
    memory::Budgeted,
};

/// Decode the stored struct's map or positional representation. Unknown map fields remain ignored, duplicate known fields fail, and nullable fields retain their absent/null default.
pub(super) fn fields<const N: usize>(
    bytes: &[u8],
    names: [(&'static str, bool); N],
    control: &StorageReadControl,
) -> StorageBackendResult<[Option<Budgeted<String>>; N]> {
    let mut reader = JsonReader::from_slice(bytes, control.memory(), control.cancellation())
        .with_ignored_string_escapes();
    let first = next(&mut reader)?;
    let object = match first {
        JsonToken::StartObject => true,
        JsonToken::StartArray => false,
        _ => return Err(invalid("expected a catalog struct")),
    };
    let mut seen = [false; N];
    let mut result = std::array::from_fn(|_| None);
    let mut position = 0;
    loop {
        let token = next(&mut reader)?;
        if matches!(
            (object, token),
            (true, JsonToken::EndObject) | (false, JsonToken::EndArray)
        ) {
            break;
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
        let value = if object { next(&mut reader)? } else { token };
        let Some(index) = selected else {
            skip(value, &mut reader)?;
            continue;
        };
        seen[index] = true;
        result[index] = match value {
            JsonToken::String(value) => Some(
                decode_json_string(value, control.memory(), control.cancellation())
                    .map_err(json_error)?,
            ),
            JsonToken::Null if names[index].1 => None,
            _ => {
                return Err(invalid(format!(
                    "invalid string field `{}`",
                    names[index].0
                )))
            }
        };
    }
    for (index, (name, nullable)) in names.into_iter().enumerate() {
        if !seen[index] && !nullable {
            return Err(invalid(format!("missing field `{name}`")));
        }
    }
    if reader.next_event().map_err(json_error)?.is_some() {
        return Err(invalid("trailing catalog value"));
    }
    control.check()?;
    Ok(result)
}

fn next<'a>(reader: &mut JsonReader<'a, '_>) -> StorageBackendResult<JsonToken<'a>> {
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

fn invalid(message: impl std::fmt::Display) -> StorageBackendError {
    StorageBackendError::Serde(serde_json::Error::custom(message))
}

fn json_error(error: JsonReadError) -> StorageBackendError {
    match error {
        JsonReadError::Memory(error) => error.into(),
        JsonReadError::Cancelled(error) => error.into(),
        error => invalid(error),
    }
}
