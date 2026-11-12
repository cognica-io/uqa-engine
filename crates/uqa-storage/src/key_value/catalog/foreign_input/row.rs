//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored foreign envelopes preserve direct fields and flattened authority before the version fence.

use crate::catalog::input::{
    relation_security,
    strings::{invalid, json_error, take},
};
use crate::{
    read_control::StorageReadControl, ForeignTableRow, RelationIdentity, StorageBackendError,
    StorageBackendResult,
};
use uqa_core::{
    json::{decode_json_string, JsonEvent, JsonReader, JsonToken},
    memory::{Budgeted, OwnedMap, ProductionControl},
};

const FIELDS: [&str; 4] = [
    "security_version",
    "server_name",
    "columns_json",
    "options_json",
];

pub(crate) fn row(
    relation: Budgeted<RelationIdentity>,
    bytes: &[u8],
    control: &StorageReadControl,
) -> StorageBackendResult<Budgeted<ForeignTableRow>> {
    control.check()?;
    let mut reader = JsonReader::from_slice(bytes, control.memory(), control.cancellation())
        .with_ignored_string_escapes();
    if next(&mut reader)?.token != JsonToken::StartObject {
        return Err(invalid("expected a foreign table catalog map"));
    }
    let mut seen = [false; 4];
    let mut version = None;
    let mut text = [None, None, None];
    let mut authority = OwnedMap::new();
    loop {
        let event = next(&mut reader)?;
        if event.token == JsonToken::EndObject {
            break;
        }
        let JsonToken::Key(name) = event.token else {
            return Err(invalid("expected a foreign table catalog field"));
        };
        let name = decode_json_string(name, control.memory(), control.cancellation())
            .map_err(json_error)?;
        let selected = FIELDS.iter().position(|field| *field == name.as_str());
        if let Some(index) = selected {
            if seen[index] {
                return Err(invalid(format!("duplicate field `{}`", FIELDS[index])));
            }
            seen[index] = true;
        }
        let event = next(&mut reader)?;
        let start = event.range.start;
        match selected {
            Some(0) => {
                let JsonToken::Number(number) = event.token else {
                    return Err(invalid("expected a foreign table security version"));
                };
                version = Some(serde_json::from_str::<u8>(number)?);
            }
            Some(index) => {
                let JsonToken::String(value) = event.token else {
                    return Err(invalid("expected a foreign table catalog string"));
                };
                text[index - 1] = Some(
                    decode_json_string(value, control.memory(), control.cancellation())
                        .map_err(json_error)?,
                );
            }
            None => (),
        }
        let end = skip(&event, &mut reader)?;
        if selected.is_none() {
            let value = &bytes[start..end];
            crate::catalog::input::strings::validate_content(value, 126, control)?;
            authority.insert_admitted_key(name, value)?;
        }
    }
    for (index, present) in seen.into_iter().enumerate() {
        if !present {
            return Err(invalid(format!("missing field `{}`", FIELDS[index])));
        }
    }
    let security = relation_security::read(&authority, control)?;
    if reader.next_event().map_err(json_error)?.is_some() {
        return Err(invalid("trailing foreign table catalog JSON"));
    }
    let version = version.expect("required security version");
    if version != super::super::STORED_FOREIGN_TABLE_SECURITY_VERSION {
        let name = relation.qualified_name_with_control(&ProductionControl::new(
            control.memory(),
            control.cancellation(),
            control.cancellation(),
        ))?;
        return Err(StorageBackendError::Other(format!(
            "foreign-table catalog record `{}` has unsupported security version {version}",
            name.as_str()
        )));
    }
    let [server_name, columns_json, options_json] =
        text.map(|value| value.expect("required foreign field"));
    let mut memory = control.memory().empty_reservation();
    let row = ForeignTableRow {
        relation: take(relation, &mut memory),
        security: take(security, &mut memory),
        server_name: take(server_name, &mut memory),
        columns_json: take(columns_json, &mut memory),
        options_json: take(options_json, &mut memory),
    };
    control.check()?;
    Ok(Budgeted::new(row, memory))
}

fn next<'a>(reader: &mut JsonReader<'a, '_>) -> StorageBackendResult<JsonEvent<'a>> {
    reader
        .next_event()
        .map_err(json_error)?
        .ok_or_else(|| invalid("unexpected end of foreign table catalog JSON"))
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
