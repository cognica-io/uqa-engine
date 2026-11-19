//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored maintenance fields become inline counters through the borrowed JSON grammar.

use super::StatisticsMaintenance;
use crate::{
    catalog::input::{metadata, strings, CatalogInputLimits},
    read_control::StorageReadControl,
    CatalogFacade, StorageBackendResult,
};
use uqa_core::{
    image_io::AsyncImageInput,
    json::{decode_json_string_with_work, JsonReadError, JsonReader, JsonToken},
    memory::{Budgeted, ProductionControl},
};

const FIELDS: [&str; 8] = [
    "object_id",
    "generation",
    "changes",
    "dirty_since_ms",
    "analyzed_rows",
    "statistics_format",
    "analyzed_at",
    "analysis_generation",
];

impl StatisticsMaintenance {
    /// Read one selected metadata record under its original allowance and cancellation. Absent state and a different stored relation identity retain the ordinary loader's default; malformed state still fails before identity selection.
    pub async fn load_for_from(
        catalog: &dyn CatalogFacade,
        table: &str,
        object_id: [u8; 16],
        limits: CatalogInputLimits,
        control: &StorageReadControl,
        input: &mut AsyncImageInput<'_>,
    ) -> StorageBackendResult<Self> {
        let Some(json) =
            metadata::read_from(catalog, Self::KEY_PREFIX, table, limits, control, input).await?
        else {
            return Ok(Self::default());
        };
        Ok(Self::from_stored_from(json, control, input)
            .await?
            .for_object(object_id))
    }

    /// Consume the admitted stored text without a JSON value tree. Known fields preserve struct defaults, positional input, duplicate rejection and unsigned integer semantics; unknown values use the original ignored-value grammar. No source payload survives the inline result.
    pub async fn from_stored_from(
        json: Budgeted<String>,
        control: &StorageReadControl,
        input: &mut AsyncImageInput<'_>,
    ) -> StorageBackendResult<Self> {
        read(&json, control, input)
            .await
            .map_err(strings::json_error)
    }
}

async fn read(
    json: &str,
    control: &StorageReadControl,
    input: &mut AsyncImageInput<'_>,
) -> Result<StatisticsMaintenance, JsonReadError> {
    let production =
        ProductionControl::new(control.memory(), control.cancellation(), input.cancellation);
    let mut work = metadata::RetainedWork { control, input };
    let mut reader = JsonReader::with_control(json, &production).with_ignored_string_escapes();
    let object = match next(&mut reader, &mut work).await? {
        JsonToken::StartObject => true,
        JsonToken::StartArray => false,
        _ => return Err(JsonReadError::InvalidJson),
    };
    let mut state = StatisticsMaintenance::default();
    let mut seen = [false; FIELDS.len()];
    let mut position = 0;
    loop {
        let token = next(&mut reader, &mut work).await?;
        if matches!(
            (object, token),
            (true, JsonToken::EndObject) | (false, JsonToken::EndArray)
        ) {
            break;
        }
        let selected = if object {
            let JsonToken::Key(key) = token else {
                return Err(JsonReadError::InvalidJson);
            };
            let key = decode_json_string_with_work(key, &production, &mut work).await?;
            FIELDS.iter().position(|name| *name == key.as_str())
        } else {
            if position == FIELDS.len() {
                return Err(JsonReadError::InvalidJson);
            }
            position += 1;
            Some(position - 1)
        };
        if selected.is_some_and(|index| seen[index]) {
            return Err(JsonReadError::InvalidJson);
        }
        let value = if object {
            next(&mut reader, &mut work).await?
        } else {
            token
        };
        let Some(index) = selected else {
            skip(value, &mut reader, &mut work).await?;
            continue;
        };
        seen[index] = true;
        match index {
            0 => state.object_id = object_id(value, &mut reader, &mut work).await?,
            1 => state.generation = number(value)?,
            2 => state.changes = number(value)?,
            3 => state.dirty_since_ms = number(value)?,
            4 => state.analyzed_rows = optional_number(value)?,
            5 => {
                state.statistics_format =
                    u32::try_from(number(value)?).map_err(|_| JsonReadError::InvalidJson)?;
            }
            6 => state.analyzed_at = optional_number(value)?,
            7 => state.analysis_generation = optional_number(value)?,
            _ => unreachable!(),
        }
    }
    if reader.next_event_with_work(&mut work).await?.is_some() {
        return Err(JsonReadError::InvalidJson);
    }
    production.check_cancellation()?;
    Ok(state)
}

async fn next<'a>(
    reader: &mut JsonReader<'a, '_>,
    input: &mut metadata::RetainedWork<'_, '_>,
) -> Result<JsonToken<'a, &'a [u8]>, JsonReadError> {
    reader
        .next_event_with_work(input)
        .await?
        .map(|event| event.token)
        .ok_or(JsonReadError::InvalidJson)
}

fn number(value: JsonToken<'_, &[u8]>) -> Result<u64, JsonReadError> {
    let JsonToken::Number(bytes) = value else {
        return Err(JsonReadError::InvalidJson);
    };
    // Valid JSON integers have no leading zeros. Every representable u64 fits in twenty bytes; the public typed decoder retains float, exponent and negative-zero rejection.
    if bytes.len() > 20 {
        return Err(JsonReadError::InvalidJson);
    }
    serde_json::from_slice(bytes).map_err(|_| JsonReadError::InvalidJson)
}

fn optional_number(value: JsonToken<'_, &[u8]>) -> Result<Option<u64>, JsonReadError> {
    if value == JsonToken::Null {
        Ok(None)
    } else {
        number(value).map(Some)
    }
}

async fn object_id(
    first: JsonToken<'_, &[u8]>,
    reader: &mut JsonReader<'_, '_>,
    input: &mut metadata::RetainedWork<'_, '_>,
) -> Result<Option<[u8; 16]>, JsonReadError> {
    if first == JsonToken::Null {
        return Ok(None);
    }
    if first != JsonToken::StartArray {
        return Err(JsonReadError::InvalidJson);
    }
    let mut identity = [0; 16];
    for byte in &mut identity {
        *byte = u8::try_from(number(next(reader, input).await?)?)
            .map_err(|_| JsonReadError::InvalidJson)?;
    }
    if next(reader, input).await? != JsonToken::EndArray {
        return Err(JsonReadError::InvalidJson);
    }
    Ok(Some(identity))
}

async fn skip(
    first: JsonToken<'_, &[u8]>,
    reader: &mut JsonReader<'_, '_>,
    input: &mut metadata::RetainedWork<'_, '_>,
) -> Result<(), JsonReadError> {
    let mut depth = usize::from(matches!(
        first,
        JsonToken::StartArray | JsonToken::StartObject
    ));
    while depth != 0 {
        match next(reader, input).await? {
            JsonToken::StartArray | JsonToken::StartObject => depth += 1,
            JsonToken::EndArray | JsonToken::EndObject => depth -= 1,
            _ => (),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
