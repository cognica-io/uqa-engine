//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrow stored index fields using Core's iterative admitted JSON grammar.

use super::{invalid, ProductionControl, SQLError};
use crate::catalog::work::Work;
use uqa_core::{
    json::{decode_json_string_with_work, JsonReadError, JsonReadWork, JsonReader, JsonToken},
    memory::Produced,
};

pub(super) struct FieldSet {
    names: &'static [&'static str],
    seen: u16,
}

impl FieldSet {
    pub(super) const fn new(names: &'static [&'static str]) -> Self {
        Self { names, seen: 0 }
    }

    pub(super) fn field(&mut self, name: &str) -> Result<usize, SQLError> {
        let position = self
            .names
            .iter()
            .position(|field| *field == name)
            .ok_or_else(invalid)?;
        let bit = 1 << position;
        if self.seen & bit != 0 {
            return Err(invalid());
        }
        self.seen |= bit;
        Ok(position)
    }

    pub(super) fn require(&self, bits: u16) -> Result<(), SQLError> {
        if self.seen & bits == bits {
            Ok(())
        } else {
            Err(invalid())
        }
    }
}

struct JsonWork<'a, W>(&'a mut W);

impl<W: Work + Send> JsonReadWork for JsonWork<'_, W> {
    type Error = JsonReadError;

    async fn checkpoint(&mut self) -> Result<(), JsonReadError> {
        self.0.checkpoint().await.map_err(Into::into)
    }
}

/// Preserve the original ordered spans and error positions while Core owns each JSON token.
pub(super) struct Input<'a, 'c> {
    text: &'a str,
    reader: JsonReader<'a, 'c>,
    control: ProductionControl<'c>,
}

impl<'a, 'c> Input<'a, 'c> {
    pub(super) async fn begin<W: Work + Send>(
        text: &'a str,
        start: JsonToken<'_, &[u8]>,
        control: &ProductionControl<'c>,
        work: &mut W,
    ) -> Result<Self, SQLError> {
        control.check()?;
        let mut reader = JsonReader::with_control(text, control);
        if reader
            .next_event_with_work(&mut JsonWork(work))
            .await
            .map_err(read_error)?
            .is_none_or(|event| event.token != start)
        {
            return Err(invalid());
        }
        Ok(Self {
            text,
            reader,
            control: *control,
        })
    }

    async fn value<W: Work + Send>(&mut self, work: &mut W) -> Result<&'a str, SQLError> {
        let span = self
            .reader
            .read_value_span_with_work(&mut JsonWork(work))
            .await
            .map_err(read_error)?;
        let end = self.reader.position().byte_offset;
        Ok(&self.text[end - span.len()..end])
    }

    async fn end<W: Work + Send>(&mut self, work: &mut W) -> Result<(), SQLError> {
        if self
            .reader
            .next_event_with_work(&mut JsonWork(work))
            .await
            .map_err(read_error)?
            .is_some()
        {
            return Err(invalid());
        }
        Ok(())
    }

    pub(super) async fn field<W: Work + Send>(
        &mut self,
        work: &mut W,
    ) -> Result<Option<(Produced<String>, &'a str)>, SQLError> {
        let event = self
            .reader
            .next_event_with_work(&mut JsonWork(work))
            .await
            .map_err(read_error)?
            .ok_or_else(invalid)?;
        match event.token {
            JsonToken::EndObject => {
                self.end(work).await?;
                Ok(None)
            }
            JsonToken::Key(key) => {
                let key = self.string(key, work).await?;
                Ok(Some((key, self.value(work).await?)))
            }
            _ => Err(invalid()),
        }
    }

    pub(super) async fn element<W: Work + Send>(
        &mut self,
        work: &mut W,
    ) -> Result<Option<&'a str>, SQLError> {
        if self
            .reader
            .next_array_value_with_work(&mut JsonWork(work))
            .await
            .map_err(read_error)?
        {
            self.value(work).await.map(Some)
        } else {
            self.end(work).await?;
            Ok(None)
        }
    }

    pub(super) async fn string<W: Work + Send>(
        &self,
        bytes: &[u8],
        work: &mut W,
    ) -> Result<Produced<String>, SQLError> {
        decode_json_string_with_work(bytes, &self.control, &mut JsonWork(work))
            .await
            .map_err(read_error)
    }
}

pub(super) async fn object_id<W: Work + Send>(
    text: &str,
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<[u8; 16], SQLError> {
    let mut result = [0; 16];
    let mut position = 0;
    let mut input = Input::begin(text, JsonToken::StartArray, control, work).await?;
    while let Some(value) = input.element(work).await? {
        *result.get_mut(position).ok_or_else(invalid)? = value.parse().map_err(|_| invalid())?;
        position += 1;
    }
    if position != result.len() {
        return Err(invalid());
    }
    Ok(result)
}

pub(super) fn boolean(text: &str) -> Result<bool, SQLError> {
    match text {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(invalid()),
    }
}

pub(super) fn require_array(text: &str) -> Result<(), SQLError> {
    if text.starts_with('[') {
        Ok(())
    } else {
        Err(invalid())
    }
}

pub(super) fn read_error(error: JsonReadError) -> SQLError {
    match error {
        JsonReadError::Memory(error) => error.into(),
        JsonReadError::Cancelled(error) => error.into(),
        JsonReadError::InvalidJson | JsonReadError::Malformed { .. } => invalid(),
    }
}
