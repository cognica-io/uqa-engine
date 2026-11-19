//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored schema envelopes select their original authority format before typed field input.

use super::{SchemaRow, SchemaTupleIdentity};
use crate::{read_control::StorageReadControl, StorageBackendError, StorageBackendResult};
use serde::de::Error as _;
use uqa_core::{
    catalog_acl::input::schema,
    json::{JsonObjectProjection, JsonReadError, JsonReader, JsonToken},
    memory::Budgeted,
};

/// Decode current and legacy stored rows with the same format discriminator as ordinary deserialization. Complete JSON syntax and envelope checks precede typed fields, including fields later overwritten by projection.
pub fn row(
    bytes: &[u8],
    control: &StorageReadControl,
) -> StorageBackendResult<Budgeted<SchemaRow>> {
    control.check()?;
    let mut reader = JsonReader::from_slice(bytes, control.memory(), control.cancellation());
    let object = reader
        .next_event()
        .map_err(json_error)?
        .map(|event| event.token)
        == Some(JsonToken::StartObject);
    drop(reader);
    if object {
        let fields = JsonObjectProjection::read(bytes, control.memory(), control.cancellation())
            .map_err(json_error)?;
        if let Some(version) = fields.get("schema_security_format") {
            let mut reader =
                JsonReader::from_slice(version, control.memory(), control.cancellation());
            let version = match reader
                .next_event()
                .map_err(json_error)?
                .map(|event| event.token)
            {
                Some(JsonToken::Number(value)) => value.parse::<u64>().ok(),
                _ => None,
            };
            if !matches!(version, Some(1 | 2)) {
                return Err(invalid("unsupported schema security format"));
            }
            if version == Some(1) && fields.get("tuple").is_some() {
                return Err(invalid("legacy schema security contains a tuple identity"));
            }
            drop(fields);
            let row = schema::projected_bound_row(bytes, control.memory(), control.cancellation())
                .map_err(json_error)?;
            if version == Some(2) && !row.tuple.is_some_and(SchemaTupleIdentity::is_valid) {
                return Err(invalid("invalid schema catalog tuple identity"));
            }
            control.check()?;
            return row.try_map(|row| Ok(SchemaRow::Bound(row)));
        }
        if fields.get("tuple").is_some() {
            return Err(invalid("schema tuple identity has no format marker"));
        }
    }
    let row = schema::projected_legacy_row(bytes, control.memory(), control.cancellation())
        .map_err(json_error)?;
    control.check()?;
    row.try_map(|row| Ok(SchemaRow::Legacy(row)))
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

#[cfg(test)]
mod tests;
