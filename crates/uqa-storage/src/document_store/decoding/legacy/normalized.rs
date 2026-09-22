//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Historical JSON normalization recognizes serde's two private object envelopes before UQA value conversion.

use uqa_core::{
    json::{decode_json_string, JsonReader, JsonToken},
    memory::Budgeted,
};

use super::{
    invalid_json, read_error, text, ContainerSpans, Member, StorageBackendResult,
    StorageReadControl,
};

#[derive(Clone, Copy)]
pub(super) enum PrivateKind {
    Number,
    Raw,
}

pub(super) fn validate(input: &str, control: &StorageReadControl) -> StorageBackendResult<()> {
    let mut reader =
        JsonReader::new(input, control.memory(), control.cancellation()).with_depth_limit(127);
    let first = reader
        .next_event()
        .map_err(read_error)?
        .ok_or_else(|| invalid_json("missing legacy JSON value"))?;
    if matches!(first.token, JsonToken::StartObject | JsonToken::StartArray) {
        match ContainerSpans::read(input.as_bytes(), control)? {
            ContainerSpans::Object(members) => {
                if let Some((kind, text)) = private_text(&members, control)? {
                    match kind {
                        PrivateKind::Number => validate_number(&text, control)?,
                        PrivateKind::Raw => validate(&text, control)?,
                    }
                } else {
                    for member in members.iter() {
                        validate(text(member.value)?, control)?;
                    }
                }
            }
            ContainerSpans::Array(values) => {
                for value in values.iter() {
                    validate(text(value)?, control)?;
                }
            }
        }
    } else if reader.next_event().map_err(read_error)?.is_some() {
        return Err(invalid_json("trailing legacy JSON value"));
    }
    Ok(())
}

pub(super) fn private_text(
    members: &[Member<'_>],
    control: &StorageReadControl,
) -> StorageBackendResult<Option<(PrivateKind, Budgeted<String>)>> {
    let Some(member) = members.first() else {
        return Ok(None);
    };
    let kind = match member.name.as_str() {
        "$serde_json::private::Number" => PrivateKind::Number,
        "$serde_json::private::RawValue" => PrivateKind::Raw,
        _ => return Ok(None),
    };
    if members.len() != 1 {
        return Err(invalid_json(
            "legacy private JSON envelope has extra fields",
        ));
    }
    let text = decode_json_string(member.value, control.memory(), control.cancellation())
        .map_err(read_error)?;
    Ok(Some((kind, text)))
}

fn validate_number(input: &str, control: &StorageReadControl) -> StorageBackendResult<()> {
    let mut reader = JsonReader::new(input, control.memory(), control.cancellation());
    let event = reader
        .next_event()
        .map_err(read_error)?
        .ok_or_else(|| invalid_json("missing private JSON number"))?;
    if !matches!(event.token, JsonToken::Number(_))
        || event.range != (0..input.len())
        || reader.next_event().map_err(read_error)?.is_some()
    {
        return Err(invalid_json("invalid private JSON number"));
    }
    Ok(())
}
