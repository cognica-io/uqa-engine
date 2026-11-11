//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered object projection retains admitted names while values borrow their original bytes.

use super::{decode_json_string, JsonReadError, JsonReader, JsonToken};
use crate::{
    memory::{MemoryBudget, OwnedMap},
    CancellationToken,
};

/// An immutable, sorted last-value projection of a complete JSON object. Names and map nodes retain their original allowances; values remain slices of the supplied text, including their original numeric spelling. No value tree or serialized copy is constructed.
pub struct JsonObjectProjection<'a> {
    fields: OwnedMap<String, &'a [u8]>,
}

impl<'a> JsonObjectProjection<'a> {
    /// Validate the complete object, including overwritten values, before returning its field projection. Strict strings and the ordinary serde JSON container-depth limit are retained. Typed consumers choose their numeric interpretation separately.
    pub fn read(
        bytes: &'a [u8],
        memory: &MemoryBudget,
        cancellation: &CancellationToken,
    ) -> Result<Self, JsonReadError> {
        let mut reader = JsonReader::from_slice(bytes, memory, cancellation).with_depth_limit(127);
        if reader.next_event()?.map(|event| event.token) != Some(JsonToken::StartObject) {
            return Err(JsonReadError::InvalidJson);
        }
        let mut fields = OwnedMap::new();
        loop {
            let event = reader.next_event()?.ok_or(JsonReadError::InvalidJson)?;
            if event.token == JsonToken::EndObject {
                break;
            }
            let JsonToken::Key(name) = event.token else {
                return Err(JsonReadError::InvalidJson);
            };
            let name = decode_json_string(name, memory, cancellation)?;
            let event = reader.next_event()?.ok_or(JsonReadError::InvalidJson)?;
            let start = event.range.start;
            let mut end = event.range.end;
            let mut depth = usize::from(matches!(
                event.token,
                JsonToken::StartArray | JsonToken::StartObject
            ));
            while depth != 0 {
                let event = reader.next_event()?.ok_or(JsonReadError::InvalidJson)?;
                match event.token {
                    JsonToken::StartArray | JsonToken::StartObject => depth += 1,
                    JsonToken::EndArray | JsonToken::EndObject => depth -= 1,
                    _ => (),
                }
                end = event.range.end;
            }
            fields.insert_admitted_key(name, &bytes[start..end])?;
        }
        if reader.next_event()?.is_some() {
            return Err(JsonReadError::InvalidJson);
        }
        cancellation.check()?;
        Ok(Self { fields })
    }

    pub fn len(&self) -> usize {
        self.fields.len()
    }

    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }

    pub fn get(&self, name: &str) -> Option<&'a [u8]> {
        self.fields.get(name).copied()
    }

    pub fn iter(&self) -> impl ExactSizeIterator<Item = (&str, &'a [u8])> {
        self.fields
            .iter()
            .map(|(name, bytes)| (name.as_str(), *bytes))
    }
}

#[cfg(test)]
mod tests;
