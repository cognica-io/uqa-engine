//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `SQLite` format consumers borrow subtrees from Core's JSON grammar, without constructing a second JSON tree.

use uqa_core::json::{JsonEvent, JsonReadError, JsonReader, JsonToken};
use uqa_storage::read_control::StorageReadControl;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    Object,
    Array,
}

pub(super) struct Item<'a> {
    pub(super) key: Option<&'a [u8]>,
    pub(super) value: &'a [u8],
}

pub(super) struct Container<'a, 'c> {
    input: &'a [u8],
    reader: JsonReader<'a, 'c>,
    pub(super) kind: Kind,
    finished: bool,
}

impl<'a, 'c> Container<'a, 'c> {
    pub(super) fn new(
        input: &'a [u8],
        control: &'c StorageReadControl,
        depth: usize,
    ) -> Result<Self, JsonReadError> {
        if depth == 0 {
            return Err(JsonReadError::InvalidJson);
        }
        // Unknown fields use serde's IgnoredAny rules; consumers strictly decode every retained key and known value themselves.
        let mut reader = JsonReader::from_slice(input, control.memory(), control.cancellation())
            .with_ignored_string_escapes();
        let kind = match next(&mut reader)?.token {
            JsonToken::StartObject => Kind::Object,
            JsonToken::StartArray => Kind::Array,
            _ => return Err(JsonReadError::InvalidJson),
        };
        Ok(Self {
            input,
            reader,
            kind,
            finished: false,
        })
    }

    pub(super) fn next(&mut self) -> Result<Option<Item<'a>>, JsonReadError> {
        if self.finished {
            return Ok(None);
        }
        let event = next(&mut self.reader)?;
        if matches!(event.token, JsonToken::EndArray | JsonToken::EndObject) {
            self.finished = true;
            if self.reader.next_event()?.is_some() {
                return Err(JsonReadError::InvalidJson);
            }
            return Ok(None);
        }
        let (key, event) = match (self.kind, event.token) {
            (Kind::Object, JsonToken::Key(key)) => (Some(key), next(&mut self.reader)?),
            (Kind::Array, _) => (None, event),
            _ => return Err(JsonReadError::InvalidJson),
        };
        let start = event.range.start;
        let mut end = event.range.end;
        let mut nesting = usize::from(matches!(
            event.token,
            JsonToken::StartArray | JsonToken::StartObject
        ));
        while nesting != 0 {
            let event = next(&mut self.reader)?;
            match event.token {
                JsonToken::StartArray | JsonToken::StartObject => nesting += 1,
                JsonToken::EndArray | JsonToken::EndObject => nesting -= 1,
                _ => {}
            }
            end = event.range.end;
        }
        Ok(Some(Item {
            key,
            value: &self.input[start..end],
        }))
    }
}

fn next<'a>(reader: &mut JsonReader<'a, '_>) -> Result<JsonEvent<'a>, JsonReadError> {
    reader.next_event()?.ok_or(JsonReadError::InvalidJson)
}

pub(super) fn scalar<'a>(
    input: &'a [u8],
    control: &StorageReadControl,
) -> Result<JsonToken<'a>, JsonReadError> {
    let mut reader = JsonReader::from_slice(input, control.memory(), control.cancellation());
    let token = next(&mut reader)?.token;
    if matches!(token, JsonToken::StartArray | JsonToken::StartObject)
        || reader.next_event()?.is_some()
    {
        return Err(JsonReadError::InvalidJson);
    }
    Ok(token)
}

pub(super) fn validate_buffered(
    input: &[u8],
    control: &StorageReadControl,
    depth: usize,
) -> Result<(), JsonReadError> {
    let mut reader = JsonReader::from_slice(input, control.memory(), control.cancellation())
        .with_depth_limit(depth);
    while reader.next_event()?.is_some() {}
    Ok(())
}
