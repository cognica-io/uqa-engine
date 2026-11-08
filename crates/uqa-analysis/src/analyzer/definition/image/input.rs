//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native configuration construction retains each decoded buffer and entry.

use std::collections::BTreeMap;
use uqa_core::{
    json::{decode_json_string, JsonReadError, JsonReader, JsonToken},
    memory::{Budgeted, BudgetedVec, MemoryBudget, MemoryReservation},
    CancellationToken,
};

pub(super) type Result<T> = std::result::Result<T, JsonReadError>;

pub(super) fn invalid() -> JsonReadError {
    JsonReadError::InvalidJson
}

pub(super) struct Input<'a, 'c> {
    reader: JsonReader<'a, 'c>,
    pending: Option<JsonToken<'a>>,
    pub(super) budget: &'c MemoryBudget,
    pub(super) cancellation: &'c CancellationToken,
    memory: MemoryReservation,
}

impl<'a, 'c> Input<'a, 'c> {
    pub(super) fn new(
        bytes: &'a [u8],
        budget: &'c MemoryBudget,
        cancellation: &'c CancellationToken,
    ) -> Result<Self> {
        cancellation.check()?;
        Ok(Self {
            reader: JsonReader::from_slice(bytes, budget, cancellation),
            pending: None,
            budget,
            cancellation,
            memory: budget.empty_reservation(),
        })
    }

    pub(super) fn finish<T>(mut self, value: T) -> Result<Budgeted<T>> {
        if self.pending.is_some() || self.reader.next_event()?.is_some() {
            return Err(invalid());
        }
        self.cancellation.check()?;
        Ok(Budgeted::new(value, self.memory))
    }

    pub(super) fn peek(&mut self) -> Result<JsonToken<'a>> {
        if self.pending.is_none() {
            self.pending = Some(self.reader.next_event()?.ok_or_else(invalid)?.token);
        }
        Ok(self.pending.expect("pending token"))
    }

    pub(super) fn next(&mut self) -> Result<JsonToken<'a>> {
        if let Some(token) = self.pending.take() {
            return Ok(token);
        }
        Ok(self.reader.next_event()?.ok_or_else(invalid)?.token)
    }

    pub(super) fn expect(&mut self, expected: JsonToken<'_>) -> Result<()> {
        if self.next()? != expected {
            return Err(invalid());
        }
        Ok(())
    }

    pub(super) fn key(&mut self, expected: &str) -> Result<()> {
        let JsonToken::Key(bytes) = self.next()? else {
            return Err(invalid());
        };
        if bytes.get(1..bytes.len() - 1) != Some(expected.as_bytes()) {
            return Err(invalid());
        }
        Ok(())
    }

    pub(super) fn has_key(&mut self, expected: &str) -> Result<bool> {
        Ok(matches!(self.peek()?, JsonToken::Key(bytes)
            if bytes.get(1..bytes.len() - 1) == Some(expected.as_bytes())))
    }

    /// The producer's enum vocabulary is ASCII and contains no escaped names.
    pub(super) fn tag(&mut self) -> Result<&'a str> {
        let JsonToken::String(bytes) = self.next()? else {
            return Err(invalid());
        };
        std::str::from_utf8(&bytes[1..bytes.len() - 1]).map_err(|_| invalid())
    }

    pub(super) fn boolean(&mut self) -> Result<bool> {
        match self.next()? {
            JsonToken::Bool(value) => Ok(value),
            _ => Err(invalid()),
        }
    }

    pub(super) fn number<T: std::str::FromStr>(&mut self) -> Result<T> {
        match self.next()? {
            JsonToken::Number(value) => value.parse().map_err(|_| invalid()),
            _ => Err(invalid()),
        }
    }

    pub(super) fn text(&mut self) -> Result<String> {
        let JsonToken::String(bytes) = self.next()? else {
            return Err(invalid());
        };
        self.copy_text(bytes)
    }

    fn copy_text(&mut self, bytes: &[u8]) -> Result<String> {
        let (text, memory) =
            decode_json_string(bytes, self.budget, self.cancellation)?.into_parts();
        self.memory.absorb(memory);
        Ok(text)
    }

    #[cfg(any(feature = "nori", feature = "kuromoji"))]
    pub(super) fn optional<T>(
        &mut self,
        read: impl FnOnce(&mut Self) -> Result<T>,
    ) -> Result<Option<T>> {
        if self.peek()? == JsonToken::Null {
            self.next()?;
            return Ok(None);
        }
        read(self).map(Some)
    }

    pub(super) fn sequence<T>(
        &mut self,
        mut read: impl FnMut(&mut Self) -> Result<T>,
    ) -> Result<Vec<T>> {
        self.expect(JsonToken::StartArray)?;
        let mut values = BudgetedVec::new(self.budget);
        while self.peek()? != JsonToken::EndArray {
            values.reserve(1)?;
            values.push(read(self)?)?;
        }
        self.next()?;
        let (values, memory) = values.into_parts();
        self.memory.absorb(memory);
        Ok(values)
    }

    pub(super) fn strings(&mut self) -> Result<Vec<String>> {
        self.sequence(Self::text)
    }

    pub(super) fn map<T>(
        &mut self,
        mut read: impl FnMut(&mut Self) -> Result<T>,
    ) -> Result<BTreeMap<String, T>> {
        self.expect(JsonToken::StartObject)?;
        let mut values = BTreeMap::new();
        while self.peek()? != JsonToken::EndObject {
            let JsonToken::Key(bytes) = self.next()? else {
                return Err(invalid());
            };
            let key = self.copy_text(bytes)?;
            if values
                .last_key_value()
                .is_some_and(|(previous, _)| previous >= &key)
            {
                return Err(invalid());
            }
            self.memory.grow(size_of::<(String, T)>())?;
            values.insert(key, read(self)?);
        }
        self.next()?;
        Ok(values)
    }
}
