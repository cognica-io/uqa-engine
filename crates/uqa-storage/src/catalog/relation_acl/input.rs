//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independently stored ACL tuples decode inside one admitted catalog record.

use super::RelationAclTuple;
use crate::{read_control::StorageReadControl, StorageBackendError, StorageBackendResult};
use serde::de::Error as _;
use uqa_core::{
    json::{decode_json_string, JsonReadError, JsonReader, JsonToken},
    memory::Budgeted,
};

impl RelationAclTuple {
    /// Decode the original tuple shape under the record input's allowance. The revision and format predicates run after complete syntax, as in ordinary decoding; only a completed valid tuple can escape.
    pub fn decode_with_control(
        bytes: &[u8],
        control: &StorageReadControl,
    ) -> StorageBackendResult<Budgeted<Self>> {
        control.check()?;
        let mut reader = JsonReader::from_slice(bytes, control.memory(), control.cancellation())
            .with_ignored_string_escapes();
        let object = match next(&mut reader)? {
            JsonToken::StartObject => true,
            JsonToken::StartArray => false,
            _ => return Err(invalid("expected an ACL tuple")),
        };
        let mut format = None;
        let mut revision = None;
        let mut acl = None;
        let mut position = 0;
        let mut seen = [false; 3];
        loop {
            let token = next(&mut reader)?;
            if matches!(
                (object, token),
                (true, JsonToken::EndObject) | (false, JsonToken::EndArray)
            ) {
                break;
            }
            let index = if object {
                let JsonToken::Key(key) = token else {
                    return Err(invalid("expected an ACL tuple field"));
                };
                let key = decode_json_string(key, control.memory(), control.cancellation())
                    .map_err(json_error)?;
                ["format", "revision", "acl"]
                    .iter()
                    .position(|field| *field == key.as_str())
                    .ok_or_else(|| invalid(format!("unknown field `{}`", key.as_str())))?
            } else {
                if position == 3 {
                    return Err(invalid("too many positional ACL tuple fields"));
                }
                position += 1;
                position - 1
            };
            if seen[index] {
                return Err(invalid(format!(
                    "duplicate field `{}`",
                    ["format", "revision", "acl"][index]
                )));
            }
            seen[index] = true;
            let value = if object { next(&mut reader)? } else { token };
            match index {
                0 => format = Some(number(value)?),
                1 => revision = Some(identity(value, &mut reader)?),
                2 => {
                    acl = Some(
                        uqa_core::catalog_acl::input::entries_from_token(
                            value,
                            &mut reader,
                            control.memory(),
                            control.cancellation(),
                        )
                        .map_err(json_error)?,
                    );
                }
                _ => unreachable!(),
            }
        }
        let format = format.ok_or_else(|| invalid("missing field `format`"))?;
        let revision = revision.ok_or_else(|| invalid("missing field `revision`"))?;
        let acl = acl.ok_or_else(|| invalid("missing field `acl`"))?;
        if reader.next_event().map_err(json_error)?.is_some() {
            return Err(invalid("trailing ACL tuple JSON"));
        }
        control.check()?;
        let (acl, memory) = acl.into_parts();
        let tuple = Budgeted::new(
            Self {
                format,
                revision,
                acl,
            },
            memory,
        );
        tuple.validate()?;
        Ok(tuple)
    }
}

fn identity(
    first: JsonToken<'_>,
    reader: &mut JsonReader<'_, '_>,
) -> StorageBackendResult<[u8; 16]> {
    if first != JsonToken::StartArray {
        return Err(invalid("expected an ACL revision array"));
    }
    let mut revision = [0; 16];
    for byte in &mut revision {
        *byte = number(next(reader)?)?;
    }
    if next(reader)? != JsonToken::EndArray {
        return Err(invalid("invalid ACL revision width"));
    }
    Ok(revision)
}

fn number<T: serde::de::DeserializeOwned>(value: JsonToken<'_>) -> StorageBackendResult<T> {
    let JsonToken::Number(value) = value else {
        return Err(invalid("expected an integer ACL tuple field"));
    };
    Ok(serde_json::from_str(value)?)
}

fn next<'a>(reader: &mut JsonReader<'a, '_>) -> StorageBackendResult<JsonToken<'a>> {
    reader
        .next_event()
        .map_err(json_error)?
        .map(|event| event.token)
        .ok_or_else(|| invalid("unexpected end of ACL tuple JSON"))
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
