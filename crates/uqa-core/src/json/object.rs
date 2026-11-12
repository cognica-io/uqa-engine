//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered object projection retains admitted names while values borrow their original bytes.

use super::{
    decode_json_string_with_work,
    work::{inline, Work},
    JsonReadError, JsonReader, JsonToken,
};
use crate::{
    image_io::AsyncImageInput,
    memory::{MemoryBudget, OwnedMap, ProductionControl},
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
        let control = ProductionControl::new(memory, cancellation, cancellation);
        inline(Self::read_with_work(bytes, &control, &mut ()))
    }

    /// Project the same object through the enclosing work channel. Both cancellation owners remain active; keys retain the supplied allowance and values continue to borrow the original bytes.
    pub async fn read_from(
        bytes: &'a [u8],
        memory: &MemoryBudget,
        original: &CancellationToken,
        input: &mut AsyncImageInput<'_>,
    ) -> Result<Self, JsonReadError> {
        let control = ProductionControl::new(memory, original, input.cancellation);
        Self::read_with_work(bytes, &control, input).await
    }

    async fn read_with_work<W: Work<Error = JsonReadError>>(
        bytes: &'a [u8],
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<Self, JsonReadError> {
        let mut reader = JsonReader::from_slice_with_control(bytes, control).with_depth_limit(127);
        if reader
            .next_event_with_work(work)
            .await?
            .map(|event| event.token)
            != Some(JsonToken::StartObject)
        {
            return Err(JsonReadError::InvalidJson);
        }
        let mut fields = OwnedMap::new();
        loop {
            let event = reader
                .next_event_with_work(work)
                .await?
                .ok_or(JsonReadError::InvalidJson)?;
            if event.token == JsonToken::EndObject {
                break;
            }
            let JsonToken::Key(name) = event.token else {
                return Err(JsonReadError::InvalidJson);
            };
            let name = decode_json_string_with_work(name, control, work)
                .await?
                .into_budgeted()
                .unwrap_or_else(|_| unreachable!("object projection requires admitted input"));
            let event = reader
                .next_event_with_work(work)
                .await?
                .ok_or(JsonReadError::InvalidJson)?;
            let start = event.range.start;
            let mut end = event.range.end;
            let mut depth = usize::from(matches!(
                event.token,
                JsonToken::StartArray | JsonToken::StartObject
            ));
            while depth != 0 {
                let event = reader
                    .next_event_with_work(work)
                    .await?
                    .ok_or(JsonReadError::InvalidJson)?;
                match event.token {
                    JsonToken::StartArray | JsonToken::StartObject => depth += 1,
                    JsonToken::EndArray | JsonToken::EndObject => depth -= 1,
                    _ => (),
                }
                end = event.range.end;
            }
            fields.insert_admitted_key(name, &bytes[start..end])?;
        }
        if reader.next_event_with_work(work).await?.is_some() {
            return Err(JsonReadError::InvalidJson);
        }
        control.check()?;
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
