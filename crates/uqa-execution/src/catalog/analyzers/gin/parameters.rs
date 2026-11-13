//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Complete string-option validation precedes the original sorted analyzer selection.

use std::ops::Range;
use uqa_core::{
    image_io::AsyncImageInput,
    json::{decode_json_string_from, JsonReadError, JsonReader, JsonToken},
    memory::Budgeted,
};

/// One selected, trimmed analyzer name keeps its original decoded allocation. Unrelated option values are validated without retaining another complete option map.
pub struct GinAnalyzerParameter {
    text: Budgeted<String>,
    name: Range<usize>,
}

impl GinAnalyzerParameter {
    /// Validate every original key and string value, including overwritten or unselected fields. Selection equals the first case-insensitive analyzer key in the original case-sensitive `BTreeMap` order; an exact duplicate takes its last value.
    pub async fn read_from(
        text: &str,
        input: &mut AsyncImageInput<'_>,
    ) -> Result<Option<Self>, JsonReadError> {
        let mut reader = JsonReader::new(text, input.memory, input.cancellation);
        if reader
            .next_event_from(input)
            .await?
            .map(|event| event.token)
            != Some(JsonToken::StartObject)
        {
            return Err(JsonReadError::InvalidJson);
        }
        let mut selected: Option<([u8; 8], Budgeted<String>)> = None;
        loop {
            let event = reader
                .next_event_from(input)
                .await?
                .ok_or(JsonReadError::InvalidJson)?;
            if event.token == JsonToken::EndObject {
                break;
            }
            let JsonToken::Key(key) = event.token else {
                return Err(JsonReadError::InvalidJson);
            };
            let key = decode_json_string_from(key, input).await?;
            let event = reader
                .next_event_from(input)
                .await?
                .ok_or(JsonReadError::InvalidJson)?;
            // The strict reader validates every original string before it can be discarded, including escaped Unicode in overwritten and unrelated values.
            let JsonToken::String(value) = event.token else {
                return Err(JsonReadError::InvalidJson);
            };
            if key.eq_ignore_ascii_case("analyzer") {
                let key: [u8; 8] = key
                    .as_bytes()
                    .try_into()
                    .expect("eight-byte analyzer option key");
                if selected
                    .as_ref()
                    .is_none_or(|(previous, _)| key <= *previous)
                {
                    selected = Some((key, decode_json_string_from(value, input).await?));
                }
            }
        }
        if reader.next_event_from(input).await?.is_some() {
            return Err(JsonReadError::InvalidJson);
        }
        let Some((_, text)) = selected else {
            return Ok(None);
        };
        let name = trim(&text, input).await?;
        Ok(Some(Self { text, name }))
    }

    pub fn name(&self) -> &str {
        &self.text[self.name.clone()]
    }
}

async fn trim(text: &str, input: &mut AsyncImageInput<'_>) -> Result<Range<usize>, JsonReadError> {
    let mut start = 0;
    for (position, character) in text.chars().enumerate() {
        if position.is_multiple_of(1024) {
            input.checkpoint().await?;
        }
        if !character.is_whitespace() {
            break;
        }
        start += character.len_utf8();
    }
    let mut end = text.len();
    for (position, character) in text[start..].chars().rev().enumerate() {
        if position.is_multiple_of(1024) {
            input.checkpoint().await?;
        }
        if !character.is_whitespace() {
            break;
        }
        end -= character.len_utf8();
    }
    input.checkpoint().await?;
    Ok(start..end)
}

#[cfg(test)]
mod tests;
