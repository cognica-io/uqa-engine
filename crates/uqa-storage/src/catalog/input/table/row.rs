//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Original flattened table fields retain stream validation and owned destination buffers.

use super::super::{relation_security as security, strings::take};

use super::{invalid, json_error, Input};
use crate::{
    read_control::StorageReadControl, RelationIdentity, StorageBackendResult, TableSchema,
};
use uqa_core::{
    json::{decode_json_string, JsonEvent, JsonReader, JsonToken},
    memory::{Budgeted, OwnedMap},
};

const TABLE_FIELDS: [&str; 8] = [
    "relation",
    "object_id",
    "storage_generation",
    "analyzer_json",
    "fts_fields",
    "vector_fields",
    "columns_json",
    "constraints_json",
];

/// Decode the original serialized table envelope. Known table fields retain direct stream semantics; flattened authority fields retain ordered last-value semantics. Field buffers enter the returned table with their original allowances.
pub fn row(
    bytes: &[u8],
    control: &StorageReadControl,
) -> StorageBackendResult<Budgeted<TableSchema>> {
    control.check()?;
    let mut reader = JsonReader::from_slice(bytes, control.memory(), control.cancellation())
        .with_ignored_string_escapes();
    if next(&mut reader)?.token != JsonToken::StartObject {
        return Err(invalid("expected a table catalog map"));
    }
    let mut seen = [false; 8];
    let mut relation = None;
    let mut object_id = [0; 16];
    let mut storage_generation = [0; 16];
    let mut analyzer = None;
    let mut fields = None;
    let mut vectors = None;
    let mut columns = empty(control);
    let mut constraints = empty(control);
    let mut authority = OwnedMap::new();
    loop {
        let event = next(&mut reader)?;
        if event.token == JsonToken::EndObject {
            break;
        }
        let JsonToken::Key(name) = event.token else {
            return Err(invalid("expected a table catalog field"));
        };
        let name = decode_json_string(name, control.memory(), control.cancellation())
            .map_err(json_error)?;
        let selected = TABLE_FIELDS
            .iter()
            .position(|expected| *expected == name.as_str());
        if let Some(index) = selected {
            if seen[index] {
                return Err(invalid(format!(
                    "duplicate field `{}`",
                    TABLE_FIELDS[index]
                )));
            }
            seen[index] = true;
        }
        let first = next(&mut reader)?;
        let start = first.range.start;
        let suffix = &bytes[start..];
        match selected {
            Some(0) => relation = Some(relation_prefix(suffix, control)?),
            Some(1) => object_id = identity_prefix(suffix)?,
            Some(2) => storage_generation = identity_prefix(suffix)?,
            Some(3) => analyzer = Some(text(first.token, control)?),
            Some(4) => {
                fields = Some(
                    Input::new(suffix, control)
                        .sequence(false, |input, token| input.text(token))?,
                );
            }
            Some(5) => vectors = Some(Input::new(suffix, control).sequence(false, Input::vector)?),
            Some(6) => columns = text(first.token, control)?,
            Some(7) => constraints = text(first.token, control)?,
            None => (),
            _ => unreachable!(),
        }
        let end = skip(&first, &mut reader)?;
        if selected.is_none() {
            let value = &bytes[start..end];
            // Flattened fields were retained as strict serde content while the enclosing table object occupied one recursion level.
            super::super::strings::validate_content(value, 126, control)?;
            authority.insert_admitted_key(name, value)?;
        }
    }
    for index in [0, 3, 4, 5] {
        if !seen[index] {
            return Err(invalid(format!("missing field `{}`", TABLE_FIELDS[index])));
        }
    }
    let security = security::read(&authority, control)?;
    if reader.next_event().map_err(json_error)?.is_some() {
        return Err(invalid("trailing table catalog JSON"));
    }
    control.check()?;
    let mut memory = control.memory().empty_reservation();
    Ok(Budgeted::new(
        TableSchema {
            relation: take(relation.expect("required relation"), &mut memory),
            security: take(security, &mut memory),
            object_id,
            storage_generation,
            analyzer_json: take(analyzer.expect("required analyzer"), &mut memory),
            fts_fields: take(fields.expect("required full-text fields"), &mut memory),
            vector_fields: take(vectors.expect("required vector fields"), &mut memory),
            columns_json: take(columns, &mut memory),
            constraints_json: take(constraints, &mut memory),
        },
        memory,
    ))
}

fn relation_prefix(
    bytes: &[u8],
    control: &StorageReadControl,
) -> StorageBackendResult<Budgeted<RelationIdentity>> {
    let ([schema, name], _) =
        super::super::strings::fields_prefix(bytes, [("schema", false), ("name", false)], control)?;
    let (schema, mut memory) = schema.expect("required schema").into_parts();
    let name = take(name.expect("required name"), &mut memory);
    Ok(Budgeted::new(RelationIdentity { schema, name }, memory))
}

fn identity_prefix(bytes: &[u8]) -> StorageBackendResult<[u8; 16]> {
    serde_json::Deserializer::from_slice(bytes)
        .into_iter::<[u8; 16]>()
        .next()
        .ok_or_else(|| invalid("missing table identity"))?
        .map_err(Into::into)
}

fn empty(control: &StorageReadControl) -> Budgeted<String> {
    Budgeted::new(String::new(), control.memory().empty_reservation())
}

fn text(
    token: JsonToken<'_>,
    control: &StorageReadControl,
) -> StorageBackendResult<Budgeted<String>> {
    let JsonToken::String(value) = token else {
        return Err(invalid("expected a table catalog string"));
    };
    decode_json_string(value, control.memory(), control.cancellation()).map_err(json_error)
}

fn next<'a>(reader: &mut JsonReader<'a, '_>) -> StorageBackendResult<JsonEvent<'a>> {
    reader
        .next_event()
        .map_err(json_error)?
        .ok_or_else(|| invalid("unexpected end of table catalog JSON"))
}

fn skip<'a>(first: &JsonEvent<'a>, reader: &mut JsonReader<'a, '_>) -> StorageBackendResult<usize> {
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
