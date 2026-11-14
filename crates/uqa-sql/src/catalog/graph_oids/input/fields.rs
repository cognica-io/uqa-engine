//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Closed stored structs accept their original named and positional Serde representations.

use super::{
    read_error, AsyncImageInput, Budgeted, JsonReader, JsonToken, ProductionControl, SQLError,
};

pub(super) struct Input<'a, 'c, 'w, 'i> {
    pub reader: JsonReader<'a, 'c>,
    pub control: ProductionControl<'c>,
    pub work: &'w mut AsyncImageInput<'i>,
}

impl<'a> Input<'a, '_, '_, '_> {
    pub fn memory(&self) -> &uqa_core::memory::MemoryBudget {
        self.control
            .budget()
            .expect("retained graph input is admitted")
    }

    pub async fn next(&mut self) -> Result<JsonToken<'a, &'a [u8]>, SQLError> {
        self.reader
            .next_event_from(self.work)
            .await
            .map_err(read_error)?
            .map(|event| event.token)
            .ok_or_else(invalid)
    }

    pub async fn text(
        &mut self,
        token: JsonToken<'_, &[u8]>,
    ) -> Result<Budgeted<String>, SQLError> {
        let (JsonToken::String(text) | JsonToken::Key(text)) = token else {
            return Err(invalid());
        };
        Ok(
            uqa_core::json::decode_json_string_with_work(text, &self.control, self.work)
                .await
                .map_err(read_error)?
                .into_budgeted()
                .unwrap_or_else(|_| unreachable!("admitted text")),
        )
    }
}

pub(super) struct Fields {
    object: bool,
    names: &'static [&'static str],
    required: u16,
    seen: u16,
    position: usize,
}

impl Fields {
    pub fn new(
        first: JsonToken<'_, &[u8]>,
        names: &'static [&'static str],
        required: u16,
    ) -> Result<Self, SQLError> {
        let object = match first {
            JsonToken::StartObject => true,
            JsonToken::StartArray => false,
            _ => return Err(invalid()),
        };
        Ok(Self {
            object,
            names,
            required,
            seen: 0,
            position: 0,
        })
    }

    pub async fn next<'a>(
        &mut self,
        input: &mut Input<'a, '_, '_, '_>,
    ) -> Result<Option<(usize, JsonToken<'a, &'a [u8]>)>, SQLError> {
        let first = input.next().await?;
        if matches!(
            (self.object, first),
            (true, JsonToken::EndObject) | (false, JsonToken::EndArray)
        ) {
            if self.seen & self.required != self.required {
                return Err(invalid());
            }
            return Ok(None);
        }
        let (index, token) = if self.object {
            if !matches!(first, JsonToken::Key(_)) {
                return Err(invalid());
            }
            let name = input.text(first).await?;
            let index = self
                .names
                .iter()
                .position(|field| *field == name.as_str())
                .ok_or_else(invalid)?;
            // Duplicate fields fail before their values are consumed, including duplicate optional nulls.
            if self.seen & (1 << index) != 0 {
                return Err(invalid());
            }
            drop(name);
            (index, input.next().await?)
        } else {
            if self.position == self.names.len() {
                return Err(invalid());
            }
            let index = self.position;
            self.position += 1;
            (index, first)
        };
        self.seen |= 1 << index;
        Ok(Some((index, token)))
    }
}

pub(super) fn number(token: JsonToken<'_, &[u8]>) -> Result<u32, SQLError> {
    let JsonToken::Number(text) = token else {
        return Err(invalid());
    };
    if text.is_empty() || text.len() > 10 || (text.len() > 1 && text[0] == b'0') {
        return Err(invalid());
    }
    text.iter().try_fold(0u32, |value, digit| {
        if !digit.is_ascii_digit() {
            return Err(invalid());
        }
        value
            .checked_mul(10)
            .and_then(|value| value.checked_add(u32::from(digit - b'0')))
            .ok_or_else(invalid)
    })
}

pub(super) fn optional(token: JsonToken<'_, &[u8]>) -> Result<Option<u32>, SQLError> {
    if token == JsonToken::Null {
        Ok(None)
    } else {
        number(token).map(Some)
    }
}

pub(super) fn invalid() -> SQLError {
    SQLError::Internal("invalid stored graph catalog OIDs".into())
}
