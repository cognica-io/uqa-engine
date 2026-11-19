//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Direct definition fields and flattened authority keep their original decoding order.

use super::super::{StoredSequence, StoredSequenceValue};
use crate::{
    catalog::{
        input::strings::{self, invalid, json_error, take},
        SequenceOptions,
    },
    read_control::StorageReadControl,
    StorageBackendResult,
};
use uqa_core::{
    json::{decode_json_string, JsonEvent, JsonReader, JsonToken},
    memory::{Budgeted, OwnedMap, ProductionControl},
};

const FIELDS: [&str; 10] = [
    "object_id",
    "definition_generation",
    "start",
    "increment",
    "current",
    "called",
    "log_count",
    "persistence",
    "owner",
    "options",
];

pub(super) fn read(
    bytes: &[u8],
    control: &StorageReadControl,
) -> StorageBackendResult<Budgeted<StoredSequence>> {
    control.check()?;
    let mut reader = JsonReader::from_slice(bytes, control.memory(), control.cancellation())
        .with_ignored_string_escapes();
    if next(&mut reader)?.token != JsonToken::StartObject {
        return Err(invalid("expected a sequence catalog map"));
    }
    let mut seen = [false; 10];
    let (mut object_id, mut definition_generation) = ([0; 16], [0; 16]);
    let (mut start, mut increment) = (0, 0);
    let (mut current, mut called, mut log_count) = (None, None, None);
    let (mut persistence, mut owner, mut options) = (None, None, None);
    let mut authority = OwnedMap::new();
    loop {
        let event = next(&mut reader)?;
        if event.token == JsonToken::EndObject {
            break;
        }
        let JsonToken::Key(key) = event.token else {
            return Err(invalid("expected a sequence catalog field"));
        };
        let key = decode_json_string(key, control.memory(), control.cancellation())
            .map_err(json_error)?;
        let selected = FIELDS.iter().position(|name| *name == key.as_str());
        if let Some(index) = selected {
            if seen[index] {
                return Err(invalid(format!("duplicate field `{}`", FIELDS[index])));
            }
            seen[index] = true;
        }
        let first = next(&mut reader)?;
        let suffix = &bytes[first.range.start..];
        match selected {
            Some(0) => object_id = fixed_prefix(suffix)?,
            Some(1) => definition_generation = fixed_prefix(suffix)?,
            Some(2) => start = fixed_prefix(suffix)?,
            Some(3) => increment = fixed_prefix(suffix)?,
            Some(4) => current = fixed_prefix(suffix)?,
            Some(5) => called = fixed_prefix(suffix)?,
            Some(6) => log_count = fixed_prefix(suffix)?,
            Some(7) => persistence = Some(text(first.token, control)?),
            Some(8) => {
                owner = uqa_core::catalog_acl::input::sequence::owner_prefix(
                    suffix,
                    control.memory(),
                    control.cancellation(),
                )
                .map_err(json_error)?;
            }
            Some(9) => options = Some(read_options(suffix, control)?),
            None => (),
            _ => unreachable!(),
        }
        let end = skip(&first, &mut reader)?;
        if selected.is_none() {
            let value = &bytes[first.range.start..end];
            strings::validate_content(value, 126, control)?;
            authority.insert_admitted_key(key, value)?;
        }
    }
    for index in [2, 3] {
        if !seen[index] {
            return Err(invalid(format!("missing field `{}`", FIELDS[index])));
        }
    }
    let security = super::authority::read(&authority, control)?;
    if reader.next_event().map_err(json_error)?.is_some() {
        return Err(invalid("trailing sequence catalog JSON"));
    }
    let persistence = persistence.map_or_else(|| copy("p", control), Ok)?;
    let options = options.map_or_else(|| default_options(control), Ok)?;
    let mut memory = control.memory().empty_reservation();
    control.check()?;
    Ok(Budgeted::new(
        StoredSequence {
            security: take(security, &mut memory),
            object_id,
            definition_generation,
            start,
            increment,
            current,
            called,
            log_count,
            persistence: take(persistence, &mut memory),
            owner,
            options: take(options, &mut memory),
        },
        memory,
    ))
}

fn read_options(
    bytes: &[u8],
    control: &StorageReadControl,
) -> StorageBackendResult<Budgeted<SequenceOptions>> {
    let (mut data_type, mut min_value, mut max_value, mut cycle, mut cache_size) =
        (None, None, None, false, 1);
    strings::fields_prefix_with(
        bytes,
        [
            ("data_type", false),
            ("min_value", true),
            ("max_value", true),
            ("cycle", false),
            ("cache_size", true),
        ],
        control,
        |index, token| {
            match index {
                0 => data_type = Some(text(token, control)?),
                1 => min_value = optional_integer(token)?,
                2 => max_value = optional_integer(token)?,
                3 => {
                    let JsonToken::Bool(value) = token else {
                        return Err(invalid("expected a sequence cycle flag"));
                    };
                    cycle = value;
                }
                4 => {
                    let JsonToken::Number(value) = token else {
                        return Err(invalid("expected a sequence cache size"));
                    };
                    cache_size = serde_json::from_str(value)?;
                }
                _ => unreachable!(),
            }
            Ok(())
        },
    )?;
    let (data_type, memory) = data_type.expect("required sequence type").into_parts();
    Ok(Budgeted::new(
        SequenceOptions {
            data_type,
            min_value,
            max_value,
            cycle,
            cache_size,
        },
        memory,
    ))
}

fn optional_integer(token: JsonToken<'_>) -> StorageBackendResult<Option<i64>> {
    match token {
        JsonToken::Null => Ok(None),
        JsonToken::Number(value) => Ok(Some(serde_json::from_str(value)?)),
        _ => Err(invalid("expected an optional sequence bound")),
    }
}

fn default_options(
    control: &StorageReadControl,
) -> StorageBackendResult<Budgeted<SequenceOptions>> {
    copy("bigint", control)?.try_map(|data_type| {
        Ok(SequenceOptions {
            data_type,
            min_value: None,
            max_value: None,
            cycle: false,
            cache_size: 1,
        })
    })
}

fn copy(text: &str, control: &StorageReadControl) -> StorageBackendResult<Budgeted<String>> {
    Ok(ProductionControl::new(
        control.memory(),
        control.cancellation(),
        control.cancellation(),
    )
    .copy_text(text)?
    .into_budgeted()
    .expect("admitted sequence text"))
}

fn text(
    token: JsonToken<'_>,
    control: &StorageReadControl,
) -> StorageBackendResult<Budgeted<String>> {
    let JsonToken::String(value) = token else {
        return Err(invalid("expected sequence text"));
    };
    decode_json_string(value, control.memory(), control.cancellation()).map_err(json_error)
}

fn fixed_prefix<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> StorageBackendResult<T> {
    serde_json::Deserializer::from_slice(bytes)
        .into_iter::<T>()
        .next()
        .ok_or_else(|| invalid("missing sequence field"))?
        .map_err(Into::into)
}

pub(super) fn value(
    bytes: &[u8],
    control: &StorageReadControl,
) -> StorageBackendResult<StoredSequenceValue> {
    let mut numbers = [0; 8];
    let mut flags = [false; 8];
    strings::fields_with(
        bytes,
        [
            ("current", false),
            ("called", false),
            ("log_count", false),
            ("increment", false),
            ("min_value", false),
            ("max_value", false),
            ("cycle", false),
            ("cache_size", false),
        ],
        control,
        |index, token| {
            if matches!(index, 1 | 6) {
                let JsonToken::Bool(value) = token else {
                    return Err(invalid("expected a sequence value flag"));
                };
                flags[index] = value;
            } else {
                let JsonToken::Number(value) = token else {
                    return Err(invalid("expected a sequence value integer"));
                };
                numbers[index] = serde_json::from_str(value)?;
            }
            Ok(())
        },
    )?;
    Ok(StoredSequenceValue {
        current: numbers[0],
        called: flags[1],
        log_count: numbers[2],
        increment: numbers[3],
        min_value: numbers[4],
        max_value: numbers[5],
        cycle: flags[6],
        cache_size: numbers[7],
    })
}

fn next<'a>(reader: &mut JsonReader<'a, '_>) -> StorageBackendResult<JsonEvent<'a>> {
    reader
        .next_event()
        .map_err(json_error)?
        .ok_or_else(|| invalid("unexpected end of sequence JSON"))
}

fn skip(first: &JsonEvent<'_>, reader: &mut JsonReader<'_, '_>) -> StorageBackendResult<usize> {
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
            _ => (),
        }
        end = event.range.end;
    }
    Ok(end)
}

#[cfg(test)]
mod tests;
