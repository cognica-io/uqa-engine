//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native configuration construction retains each decoded buffer and entry.

use crate::configuration::{Item, Read};
use serde::de::DeserializeOwned;
use std::collections::BTreeMap;
use uqa_core::{
    image_io::AsyncImageInput,
    json::{decode_json_string_from, JsonReadError, JsonReader, JsonToken},
    memory::{Budgeted, BudgetedVec, MemoryBudget, MemoryReservation},
    CancellationToken,
};

pub(super) type Result<T> = std::result::Result<T, JsonReadError>;

pub(super) fn invalid() -> JsonReadError {
    JsonReadError::InvalidJson
}

pub(super) struct Input<'a, 'c> {
    reader: JsonReader<'a, 'c>,
    pending: Option<JsonToken<'a, &'a [u8]>>,
    work: AsyncImageInput<'c>,
    pub(super) budget: &'c MemoryBudget,
    pub(super) cancellation: &'c CancellationToken,
    memory: MemoryReservation,
}

impl<'a, 'c> Input<'a, 'c> {
    pub(super) fn new(bytes: &'a [u8], work: AsyncImageInput<'c>) -> Result<Self> {
        let budget = work.memory;
        let cancellation = work.cancellation;
        cancellation.check()?;
        Ok(Self {
            reader: JsonReader::from_slice(bytes, budget, cancellation),
            pending: None,
            work,
            budget,
            cancellation,
            memory: budget.empty_reservation(),
        })
    }

    pub(super) async fn finish<T>(mut self, value: T) -> Result<Budgeted<T>> {
        if self.pending.is_some() || self.reader.next_event_from(&mut self.work).await?.is_some() {
            return Err(invalid());
        }
        self.cancellation.check()?;
        Ok(Budgeted::new(value, self.memory))
    }

    pub(super) async fn peek(&mut self) -> Result<JsonToken<'a, &'a [u8]>> {
        if self.pending.is_none() {
            self.pending = Some(
                self.reader
                    .next_event_from(&mut self.work)
                    .await?
                    .ok_or_else(invalid)?
                    .token,
            );
        }
        Ok(self.pending.expect("pending token"))
    }

    pub(super) async fn next(&mut self) -> Result<JsonToken<'a, &'a [u8]>> {
        if let Some(token) = self.pending.take() {
            return Ok(token);
        }
        Ok(self
            .reader
            .next_event_from(&mut self.work)
            .await?
            .ok_or_else(invalid)?
            .token)
    }

    pub(super) async fn expect(&mut self, expected: JsonToken<'_, &[u8]>) -> Result<()> {
        if self.next().await? != expected {
            return Err(invalid());
        }
        Ok(())
    }

    pub(super) async fn key(&mut self, expected: &str) -> Result<()> {
        let JsonToken::Key(bytes) = self.next().await? else {
            return Err(invalid());
        };
        if bytes.get(1..bytes.len() - 1) != Some(expected.as_bytes()) {
            return Err(invalid());
        }
        Ok(())
    }

    /// The producer's enum vocabulary is ASCII and contains no escaped names.
    pub(super) async fn tag(&mut self) -> Result<&'a str> {
        let JsonToken::String(bytes) = self.next().await? else {
            return Err(invalid());
        };
        // Native discriminator names are a fixed ASCII vocabulary shorter than 64 bytes.
        if bytes.len() > 66 {
            return Err(invalid());
        }
        std::str::from_utf8(&bytes[1..bytes.len() - 1]).map_err(|_| invalid())
    }

    pub(super) async fn text(&mut self) -> Result<String> {
        let JsonToken::String(bytes) = self.next().await? else {
            return Err(invalid());
        };
        self.copy_text(bytes).await
    }

    async fn copy_text(&mut self, bytes: &[u8]) -> Result<String> {
        let (text, memory) = decode_json_string_from(bytes, &mut self.work)
            .await?
            .into_parts();
        self.memory.absorb(memory);
        Ok(text)
    }

    pub(super) async fn sequence<T: Item>(&mut self) -> Result<Vec<T>> {
        self.expect(JsonToken::StartArray).await?;
        let mut values = BudgetedVec::new(self.budget);
        while self.peek().await? != JsonToken::EndArray {
            values.reserve(1)?;
            values.push(T::read(self).await?)?;
        }
        self.next().await?;
        let (values, memory) = values.into_parts();
        self.memory.absorb(memory);
        Ok(values)
    }

    pub(super) async fn map<T: Item>(&mut self) -> Result<BTreeMap<String, T>> {
        self.expect(JsonToken::StartObject).await?;
        let mut values: BTreeMap<String, T> = BTreeMap::new();
        while self.peek().await? != JsonToken::EndObject {
            let JsonToken::Key(bytes) = self.next().await? else {
                return Err(invalid());
            };
            let key = self.copy_text(bytes).await?;
            if let Some((previous, _)) = values.last_key_value() {
                if !self.increasing(previous, &key).await? {
                    return Err(invalid());
                }
            }
            self.memory.grow(size_of::<(String, T)>())?;
            values.insert(key, T::read(self).await?);
        }
        self.next().await?;
        Ok(values)
    }

    async fn increasing(&mut self, previous: &str, key: &str) -> Result<bool> {
        for (left, right) in previous
            .as_bytes()
            .chunks(4096)
            .zip(key.as_bytes().chunks(4096))
        {
            self.work.checkpoint().await?;
            match left.cmp(right) {
                std::cmp::Ordering::Less => return Ok(true),
                std::cmp::Ordering::Greater => return Ok(false),
                std::cmp::Ordering::Equal => (),
            }
        }
        Ok(previous.len() < key.len())
    }
}

impl Read for Input<'_, '_> {
    type Error = JsonReadError;
    fn error(&self, _: serde_json::Error) -> Self::Error {
        invalid()
    }
    fn implicit_defaults(&self) -> bool {
        false
    }
    fn missing(&self, _: &'static str) -> Self::Error {
        invalid()
    }
    #[cfg(any(feature = "nori", feature = "kuromoji"))]
    fn untagged(&self, error: Self::Error, _: &'static str) -> Self::Error {
        error
    }
    #[cfg(any(feature = "nori", feature = "kuromoji"))]
    async fn is_text(&mut self) -> Result<bool> {
        Ok(matches!(self.peek().await?, JsonToken::String(_)))
    }

    async fn object(&mut self, _: &'static str) -> Result<()> {
        self.expect(JsonToken::StartObject).await
    }
    async fn tagged(
        &mut self,
        _: &'static str,
        tag: &'static str,
        variants: &'static [&'static str],
    ) -> Result<&'static str> {
        self.expect(JsonToken::StartObject).await?;
        self.key(tag).await?;
        let tag = self.tag().await?;
        variants
            .iter()
            .copied()
            .find(|variant| *variant == tag)
            .ok_or_else(invalid)
    }
    async fn field(
        &mut self,
        names: &'static [&'static str],
        omitted: &'static [bool],
        _: &'static str,
        _: bool,
        previous: &mut Option<usize>,
    ) -> Result<Option<usize>> {
        match self.next().await? {
            JsonToken::EndObject => {
                let next = previous.map_or(0, |index| index + 1);
                if omitted[next..].iter().any(|value| !value) {
                    return Err(invalid());
                }
                Ok(None)
            }
            JsonToken::Key(bytes) => {
                let index = names
                    .iter()
                    .position(|name| Some(name.as_bytes()) == bytes.get(1..bytes.len() - 1))
                    .ok_or_else(invalid)?;
                if previous.is_some_and(|last| index <= last) {
                    return Err(invalid());
                }
                let next = previous.map_or(0, |index| index + 1);
                if omitted[next..index].iter().any(|value| !value) {
                    return Err(invalid());
                }
                *previous = Some(index);
                Ok(Some(index))
            }
            _ => Err(invalid()),
        }
    }
    async fn text(&mut self, _: &'static str) -> Result<String> {
        Input::text(self).await
    }
    async fn atom<T: DeserializeOwned + Send>(&mut self) -> Result<T> {
        match self.next().await? {
            JsonToken::Bool(value) => {
                serde_json::from_value(serde_json::Value::Bool(value)).map_err(|_| invalid())
            }
            JsonToken::Number(value) if value.len() <= 40 => {
                serde_json::from_slice(value).map_err(|_| invalid())
            }
            JsonToken::String(value) if value.len() <= 66 && !value.contains(&b'\\') => {
                serde_json::from_slice(value).map_err(|_| invalid())
            }
            _ => Err(invalid()),
        }
    }
    async fn null(&mut self) -> Result<bool> {
        if self.peek().await? == JsonToken::Null {
            self.next().await?;
            Ok(true)
        } else {
            Ok(false)
        }
    }
    async fn sequence<T: Item>(&mut self) -> Result<Vec<T>> {
        Input::sequence(self).await
    }
    async fn map<T: Item>(&mut self) -> Result<BTreeMap<String, T>> {
        Input::map(self).await
    }
}
