//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored JSON lends raw fields to the common native constructors under one allowance.

use super::{Item, Read};
use crate::Analyzer;
use serde::de::DeserializeOwned;
use std::collections::BTreeMap;
use uqa_core::{
    image_io::AsyncImageInput,
    json::{decode_json_string_from, JsonReadError, JsonToken},
    memory::{Budgeted, BudgetedVec, MemoryReservation},
};

mod cursor;
mod fields;
mod numbers;
use cursor::{Cursor, Raw};
use fields::Fields;

type Result<T> = std::result::Result<T, JsonReadError>;
fn invalid() -> JsonReadError {
    JsonReadError::InvalidJson
}

struct Input<'a, 'w, 'c> {
    current: Raw<'a>,
    fields: BudgetedVec<Fields<'a, 'c>>,
    memory: MemoryReservation,
    work: &'w mut AsyncImageInput<'c>,
}

pub(crate) async fn decode_from(
    text: &str,
    work: &mut AsyncImageInput<'_>,
) -> Result<Budgeted<Analyzer>> {
    let mut input = Input::new(Raw::json(text, false, 128), work);
    let value = super::analyzer(&mut input).await?;
    input.finish(value)
}

impl<'a, 'w, 'c> Input<'a, 'w, 'c> {
    fn new(current: Raw<'a>, work: &'w mut AsyncImageInput<'c>) -> Self {
        Self {
            current,
            fields: BudgetedVec::new(work.memory),
            memory: work.memory.empty_reservation(),
            work,
        }
    }

    fn finish<T>(self, value: T) -> Result<Budgeted<T>> {
        self.work.cancellation.check()?;
        if !self.fields.is_empty() {
            return Err(invalid());
        }
        Ok(Budgeted::new(value, self.memory))
    }

    async fn child<T: Item>(&mut self, current: Raw<'a>) -> Result<Budgeted<T>> {
        let mut input = Input::new(current, self.work);
        let value = T::read(&mut input).await?;
        input.finish(value)
    }

    async fn native_text(&mut self) -> Result<String> {
        let value = match self.current {
            Raw::NumberText(text) => numbers::text(text, self.work).await?,
            raw @ Raw::Json { .. } => {
                let mut cursor = Cursor::scalar(raw, self.work)?;
                let JsonToken::String(bytes) = cursor.next(self.work).await?.token else {
                    return Err(invalid());
                };
                let value = decode_json_string_from(bytes, self.work).await?;
                cursor.end(self.work).await?;
                value
            }
        };
        let (value, memory) = value.into_parts();
        self.memory.absorb(memory);
        Ok(value)
    }

    async fn native_map<T: Item>(&mut self) -> Result<BTreeMap<String, T>> {
        if let Raw::Json {
            text,
            content: true,
            ..
        } = self.current
        {
            let mut scalar = Cursor::scalar(self.current, self.work)?;
            if matches!(scalar.next(self.work).await?.token, JsonToken::Number(_))
                && numbers::is_map(text.trim())
            {
                scalar.end(self.work).await?;
                let key = numbers::key(self.work).await?;
                let value = self.child::<T>(Raw::NumberText(text.trim())).await?;
                self.memory.grow(size_of::<(String, T)>())?;
                let (key, key_memory) = key.into_parts();
                let (value, value_memory) = value.into_parts();
                self.memory.absorb(key_memory);
                self.memory.absorb(value_memory);
                let mut values = BTreeMap::new();
                values.insert(key, value);
                return Ok(values);
            }
        }
        let mut cursor = Cursor::container(self.current, self.work, JsonToken::StartObject).await?;
        let mut entries = Entries::new(self.work.memory.empty_reservation());
        loop {
            let event = cursor.next(self.work).await?;
            if event.token == JsonToken::EndObject {
                break;
            }
            let JsonToken::Key(bytes) = event.token else {
                return Err(invalid());
            };
            let key = decode_json_string_from(bytes, self.work).await?;
            let event = cursor.next(self.work).await?;
            let raw = cursor.value(event, self.work).await?;
            let value = self.child::<T>(raw).await?;
            entries.insert(key, value)?;
        }
        cursor.end(self.work).await?;
        let mut values = BTreeMap::new();
        while let Some((key, (value, key_memory))) = entries.values.pop_first() {
            self.work.checkpoint().await?;
            drop(
                entries
                    .memory
                    .split(size_of::<(String, Budgeted<T>, MemoryReservation)>()),
            );
            self.memory.grow(size_of::<(String, T)>())?;
            let (value, value_memory) = value.into_parts();
            self.memory.absorb(key_memory);
            self.memory.absorb(value_memory);
            values.insert(key, value);
        }
        Ok(values)
    }
}

struct Entries<T> {
    values: BTreeMap<String, (Budgeted<T>, MemoryReservation)>,
    memory: MemoryReservation,
}

impl<T> Entries<T> {
    fn new(memory: MemoryReservation) -> Self {
        Self {
            values: BTreeMap::new(),
            memory,
        }
    }

    fn insert(&mut self, key: Budgeted<String>, value: Budgeted<T>) -> Result<()> {
        let entry_bytes = size_of::<(String, Budgeted<T>, MemoryReservation)>();
        if let Some(previous) = self.values.remove_entry(key.as_str()) {
            drop(previous);
            drop(self.memory.split(entry_bytes));
        }
        self.memory.grow(entry_bytes)?;
        let (key, memory) = key.into_parts();
        self.values.insert(key, (value, memory));
        Ok(())
    }
}

impl Read for Input<'_, '_, '_> {
    type Error = JsonReadError;
    fn error(&self, _: serde_json::Error) -> Self::Error {
        invalid()
    }
    fn implicit_defaults(&self) -> bool {
        true
    }
    fn missing(&self, _: &'static str) -> Self::Error {
        invalid()
    }
    #[cfg(any(feature = "nori", feature = "kuromoji"))]
    async fn default_text(&mut self, value: &'static str) -> Result<String> {
        let (value, memory) = self.work.copy_text(value).await?.into_parts();
        self.memory.absorb(memory);
        Ok(value)
    }
    #[cfg(any(feature = "nori", feature = "kuromoji"))]
    fn untagged(&self, error: Self::Error, _: &'static str) -> Self::Error {
        error
    }
    #[cfg(any(feature = "nori", feature = "kuromoji"))]
    async fn is_text(&mut self) -> Result<bool> {
        if matches!(self.current, Raw::NumberText(_)) {
            return Ok(true);
        }
        Ok(matches!(
            Cursor::scalar(self.current, self.work)?
                .next(self.work)
                .await?
                .token,
            JsonToken::String(_)
        ))
    }
    async fn object(&mut self, _: &'static str) -> Result<()> {
        let fields = Fields::open(self.current, None, self.work).await?;
        self.fields.push(fields)?;
        Ok(())
    }
    async fn tagged(
        &mut self,
        _: &'static str,
        tag: &'static str,
        variants: &'static [&'static str],
    ) -> Result<&'static str> {
        let selected = fields::tag(self.current, tag, variants, self.work).await?;
        let fields = Fields::open(self.current.content(), Some(tag), self.work).await?;
        self.fields.push(fields)?;
        Ok(selected)
    }
    async fn field(
        &mut self,
        names: &'static [&'static str],
        _: &'static [bool],
        _: &'static str,
        deny: bool,
        _: &mut Option<usize>,
    ) -> Result<Option<usize>> {
        if let Some((index, current)) = self
            .fields
            .last_mut()
            .ok_or_else(invalid)?
            .next(names, deny, self.work)
            .await?
        {
            self.current = current;
            Ok(Some(index))
        } else {
            self.fields.pop();
            Ok(None)
        }
    }
    async fn text(&mut self, _: &'static str) -> Result<String> {
        self.native_text().await
    }
    async fn atom<T: DeserializeOwned + Send>(&mut self) -> Result<T> {
        numbers::atom(self.current, self.work).await
    }
    async fn null(&mut self) -> Result<bool> {
        if matches!(self.current, Raw::NumberText(_)) {
            return Ok(false);
        }
        let mut cursor = Cursor::scalar(self.current, self.work)?;
        if cursor.next(self.work).await?.token == JsonToken::Null {
            cursor.end(self.work).await?;
            Ok(true)
        } else {
            Ok(false)
        }
    }
    async fn sequence<T: Item>(&mut self) -> Result<Vec<T>> {
        let mut cursor = Cursor::container(self.current, self.work, JsonToken::StartArray).await?;
        let mut values = BudgetedVec::new(self.work.memory);
        loop {
            let event = cursor.next(self.work).await?;
            if event.token == JsonToken::EndArray {
                break;
            }
            values.reserve(1)?;
            let raw = cursor.value(event, self.work).await?;
            let (value, memory) = self.child::<T>(raw).await?.into_parts();
            self.memory.absorb(memory);
            values.push(value)?;
        }
        cursor.end(self.work).await?;
        let (values, memory) = values.into_parts();
        self.memory.absorb(memory);
        Ok(values)
    }
    async fn map<T: Item>(&mut self) -> Result<BTreeMap<String, T>> {
        self.native_map().await
    }
}
