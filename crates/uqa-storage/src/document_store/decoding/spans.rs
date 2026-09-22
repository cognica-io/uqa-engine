//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Storage format descriptors borrow complete subtrees from the shared JSON event grammar.

use uqa_core::{
    json::{decode_json_string, JsonEvent, JsonReader, JsonToken},
    memory::{Budgeted, BudgetedVec},
};

use super::{invalid_json, read_error, StorageBackendResult, StorageReadControl};

pub(crate) struct Member<'a> {
    pub(crate) name: Budgeted<String>,
    pub(super) encoded_name: &'a [u8],
    pub(crate) value: &'a [u8],
    pub(super) position: usize,
}

pub(crate) enum ContainerSpans<'a> {
    Object(BudgetedVec<Member<'a>>),
    Array(BudgetedVec<&'a [u8]>),
}

impl<'a> ContainerSpans<'a> {
    pub(crate) fn read(
        input: &'a [u8],
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        let mut reader = JsonReader::from_slice(input, control.memory(), control.cancellation())
            .with_depth_limit(127);
        Self::collect(input, control, &mut reader)
    }

    pub(crate) fn read_envelope(
        input: &'a [u8],
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        let mut reader = JsonReader::from_slice(input, control.memory(), control.cancellation())
            .with_ignored_string_escapes();
        Self::collect(input, control, &mut reader)
    }

    fn collect(
        input: &'a [u8],
        control: &StorageReadControl,
        reader: &mut JsonReader<'a, '_>,
    ) -> StorageBackendResult<Self> {
        let event = next(reader)?;
        let result = match event.token {
            JsonToken::StartObject => {
                let mut members = BudgetedVec::new(control.memory());
                loop {
                    let event = next(reader)?;
                    if event.token == JsonToken::EndObject {
                        break;
                    }
                    let JsonToken::Key(encoded) = event.token else {
                        return Err(invalid_json("expected document object field"));
                    };
                    let name =
                        decode_json_string(encoded, control.memory(), control.cancellation())
                            .map_err(read_error)?;
                    let position = event.range.start;
                    let event = next(reader)?;
                    let value = subtree(input, reader, &event)?;
                    members.push(Member {
                        name,
                        encoded_name: encoded,
                        value,
                        position,
                    })?;
                }
                Self::Object(members)
            }
            JsonToken::StartArray => {
                let mut values = BudgetedVec::new(control.memory());
                loop {
                    let event = next(reader)?;
                    if event.token == JsonToken::EndArray {
                        break;
                    }
                    values.push(subtree(input, reader, &event)?)?;
                }
                Self::Array(values)
            }
            _ => return Err(invalid_json("expected document container")),
        };
        if reader.next_event().map_err(read_error)?.is_some() {
            return Err(invalid_json("trailing document container"));
        }
        Ok(result)
    }
}

fn next<'a>(reader: &mut JsonReader<'a, '_>) -> StorageBackendResult<JsonEvent<'a>> {
    reader
        .next_event()
        .map_err(read_error)?
        .ok_or_else(|| invalid_json("incomplete document container"))
}

fn subtree<'a>(
    input: &'a [u8],
    reader: &mut JsonReader<'a, '_>,
    event: &JsonEvent<'a>,
) -> StorageBackendResult<&'a [u8]> {
    let start = event.range.start;
    let mut end = event.range.end;
    let mut depth = usize::from(matches!(
        event.token,
        JsonToken::StartArray | JsonToken::StartObject
    ));
    while depth != 0 {
        let event = next(reader)?;
        match event.token {
            JsonToken::StartArray | JsonToken::StartObject => depth += 1,
            JsonToken::EndArray | JsonToken::EndObject => depth -= 1,
            _ => {}
        }
        end = event.range.end;
    }
    Ok(&input[start..end])
}
