//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored label fields adopt their admitted strings into the existing native registry.

use super::NativeLabels;
use crate::{GraphStoreError, LabelKind};
use uqa_core::{
    image_io::AsyncImageInput,
    json::{decode_json_string_with_work, JsonReadError, JsonReadWork, JsonReader, JsonToken},
    memory::{Budgeted, ProductionControl},
};
use uqa_storage::{read_control::StorageReadControl, StorageBackendError, StorageBackendResult};

mod fields;
#[cfg(test)]
mod tests;

type Token<'a> = JsonToken<'a, &'a [u8]>;

pub(super) struct Input<'a, 'c> {
    reader: JsonReader<'a, 'c>,
    control: ProductionControl<'c>,
}

pub(crate) struct Work<'a, 'i> {
    pub control: &'a StorageReadControl,
    pub input: &'a mut AsyncImageInput<'i>,
}

impl JsonReadWork for Work<'_, '_> {
    type Error = JsonReadError;

    async fn checkpoint(&mut self) -> Result<(), JsonReadError> {
        self.control.cancellation().check()?;
        self.input.checkpoint().await?;
        self.control.cancellation().check()?;
        self.input.cancellation.check()?;
        Ok(())
    }
}

impl NativeLabels {
    /// Consume the original map or positional representation without a temporary JSON tree or a second native publication pass. Absent and empty catalog values retain the ordinary default registry.
    pub(crate) async fn read_stored_from(
        text: Option<&str>,
        control: &StorageReadControl,
        input: &mut AsyncImageInput<'_>,
    ) -> StorageBackendResult<Self> {
        let mut work = Work { control, input };
        work.checkpoint().await.map_err(error)?;
        let Some(text) = text.filter(|text| !text.is_empty()) else {
            return Ok(Self::new(control.memory()));
        };
        let production = ProductionControl::new(
            control.memory(),
            control.cancellation(),
            work.input.cancellation,
        );
        let mut source = Input::new(text, production);
        let mut result = Self::new(control.memory());
        let mut fields = fields::Fields::new(source.next(&mut work).await?)?;
        while let Some((field, token)) = fields.next(&mut source, &mut work).await? {
            match field {
                0 => result.allocation_id = source.identity(token, &mut work).await?,
                1 => source.names(token, &mut result.labels, &mut work).await?,
                2 => source.kinds(token, &mut result.kinds, &mut work).await?,
                3 => {
                    source
                        .sequences(token, &mut result.sequences, &mut work)
                        .await?;
                }
                4 => {
                    source
                        .dropped(token, &mut result.dropped_label_ids, &mut work)
                        .await?;
                }
                5 => result.next_label_id = unsigned(token)?,
                _ => unreachable!("known graph registry field"),
            }
        }
        source.end(&mut work).await?;
        Ok(result)
    }

    pub(crate) async fn read_allocation_identity_from(
        text: Option<&str>,
        control: &StorageReadControl,
        input: &mut AsyncImageInput<'_>,
    ) -> StorageBackendResult<[u8; 16]> {
        let mut work = Work { control, input };
        work.checkpoint().await.map_err(error)?;
        let Some(text) = text else {
            return Ok([0; 16]);
        };
        let production = ProductionControl::new(
            control.memory(),
            control.cancellation(),
            work.input.cancellation,
        );
        let mut source = Input::new(text, production);
        let first = source.next(&mut work).await?;
        let identity = source.identity(first, &mut work).await?;
        source.end(&mut work).await?;
        Ok(identity)
    }
}

impl<'a, 'c> Input<'a, 'c> {
    fn new(text: &'a str, control: ProductionControl<'c>) -> Self {
        Self {
            reader: JsonReader::with_control(text, &control).with_ignored_string_escapes(),
            control,
        }
    }

    async fn next(&mut self, work: &mut Work<'_, '_>) -> StorageBackendResult<Token<'a>> {
        self.reader
            .next_event_with_work(work)
            .await
            .map_err(error)?
            .map(|event| event.token)
            .ok_or_else(invalid)
    }

    async fn end(&mut self, work: &mut Work<'_, '_>) -> StorageBackendResult<()> {
        if self
            .reader
            .next_event_with_work(work)
            .await
            .map_err(error)?
            .is_some()
        {
            return Err(invalid());
        }
        self.control.check()?;
        Ok(())
    }

    async fn text(
        &self,
        token: Token<'_>,
        work: &mut Work<'_, '_>,
    ) -> StorageBackendResult<Budgeted<String>> {
        let (JsonToken::String(bytes) | JsonToken::Key(bytes)) = token else {
            return Err(invalid());
        };
        Ok(decode_json_string_with_work(bytes, &self.control, work)
            .await
            .map_err(error)?
            .into_budgeted()
            .expect("stored graph fields retain their original allowance"))
    }

    async fn identity(
        &mut self,
        first: Token<'a>,
        work: &mut Work<'_, '_>,
    ) -> StorageBackendResult<[u8; 16]> {
        if first != JsonToken::StartArray {
            return Err(invalid());
        }
        let mut identity = [0; 16];
        for byte in &mut identity {
            *byte = unsigned(self.next(work).await?)?;
        }
        if self.next(work).await? != JsonToken::EndArray {
            return Err(invalid());
        }
        Ok(identity)
    }

    async fn names(
        &mut self,
        first: Token<'a>,
        result: &mut super::super::NativeMap<String, u32>,
        work: &mut Work<'_, '_>,
    ) -> StorageBackendResult<()> {
        require_object(first)?;
        while let Some(name) = self.key(work).await? {
            let value = unsigned(self.next(work).await?)?;
            let bytes = name.capacity();
            let (name, memory) = name.into_parts();
            result.insert_admitted(name, value, memory, bytes)?;
        }
        Ok(())
    }

    async fn kinds(
        &mut self,
        first: Token<'a>,
        result: &mut super::super::NativeMap<String, LabelKind>,
        work: &mut Work<'_, '_>,
    ) -> StorageBackendResult<()> {
        require_object(first)?;
        while let Some(name) = self.key(work).await? {
            let first = self.next(work).await?;
            let object = first == JsonToken::StartObject;
            let token = if object {
                self.next(work).await?
            } else {
                first
            };
            let kind = match self.text(token, work).await?.as_str() {
                "v" => LabelKind::Vertex,
                "e" => LabelKind::Edge,
                _ => return Err(invalid()),
            };
            if object
                && (self.next(work).await? != JsonToken::Null
                    || self.next(work).await? != JsonToken::EndObject)
            {
                return Err(invalid());
            }
            let bytes = name.capacity();
            let (name, memory) = name.into_parts();
            result.insert_admitted(name, kind, memory, bytes)?;
        }
        Ok(())
    }

    async fn key(
        &mut self,
        work: &mut Work<'_, '_>,
    ) -> StorageBackendResult<Option<Budgeted<String>>> {
        let token = self.next(work).await?;
        match token {
            JsonToken::EndObject => Ok(None),
            JsonToken::Key(_) => self.text(token, work).await.map(Some),
            _ => Err(invalid()),
        }
    }

    async fn sequences(
        &mut self,
        first: Token<'a>,
        result: &mut super::super::NativeMap<u32, u64>,
        work: &mut Work<'_, '_>,
    ) -> StorageBackendResult<()> {
        require_object(first)?;
        loop {
            let token = self.next(work).await?;
            if token == JsonToken::EndObject {
                return Ok(());
            }
            let JsonToken::Key(raw) = token else {
                return Err(invalid());
            };
            // Numeric map keys use the original raw numeric grammar, including rejection of escaped digits and whitespace inside the quotes.
            let id = unsigned(JsonToken::Number(&raw[1..raw.len() - 1]))?;
            let value = unsigned(self.next(work).await?)?;
            result.insert(id, value, 0, 0)?;
        }
    }

    async fn dropped(
        &mut self,
        first: Token<'a>,
        result: &mut super::super::NativeSet<u32>,
        work: &mut Work<'_, '_>,
    ) -> StorageBackendResult<()> {
        if first != JsonToken::StartArray {
            return Err(invalid());
        }
        loop {
            let token = self.next(work).await?;
            if token == JsonToken::EndArray {
                return Ok(());
            }
            result.insert(unsigned(token)?, 0)?;
        }
    }
}

fn require_object(token: Token<'_>) -> StorageBackendResult<()> {
    if token == JsonToken::StartObject {
        Ok(())
    } else {
        Err(invalid())
    }
}

fn unsigned<T: TryFrom<u64>>(token: Token<'_>) -> StorageBackendResult<T> {
    let JsonToken::Number(bytes) = token else {
        return Err(invalid());
    };
    if bytes.is_empty() || bytes.len() > 20 || (bytes.len() > 1 && bytes[0] == b'0') {
        return Err(invalid());
    }
    let value = bytes
        .iter()
        .try_fold(0_u64, |value, byte| {
            if !byte.is_ascii_digit() {
                return None;
            }
            value.checked_mul(10)?.checked_add(u64::from(byte - b'0'))
        })
        .ok_or_else(invalid)?;
    T::try_from(value).map_err(|_| invalid())
}

fn invalid() -> StorageBackendError {
    StorageBackendError::backend(
        "graph catalog",
        GraphStoreError::CorruptGraph("invalid stored graph registry JSON".into()),
    )
}

fn error(error: JsonReadError) -> StorageBackendError {
    match error {
        JsonReadError::Memory(error) => error.into(),
        JsonReadError::Cancelled(error) => error.into(),
        JsonReadError::InvalidJson | JsonReadError::Malformed { .. } => invalid(),
    }
}
