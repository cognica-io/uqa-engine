//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{invalid, number, Input, InputError, Integer, JsonToken, Produced};
use serde::de::{value::StrDeserializer, DeserializeOwned};
use uqa_core::memory::ProductionVec;

impl Integer for u8 {
    const MAX_LITERAL_BYTES: usize = 3;
}

impl Integer for i16 {
    const MAX_LITERAL_BYTES: usize = 6;
}

impl Integer for usize {
    const MAX_LITERAL_BYTES: usize = usize::MAX.ilog10() as usize + 1;
}

impl Integer for u64 {
    const MAX_LITERAL_BYTES: usize = 20;
}

impl Integer for i64 {
    const MAX_LITERAL_BYTES: usize = 20;
}

pub(crate) fn boolean(token: JsonToken<'_>) -> Result<bool, InputError> {
    let JsonToken::Bool(value) = token else {
        return Err(invalid());
    };
    Ok(value)
}

pub(crate) fn unit<T: DeserializeOwned + Copy>(tag: &str) -> Result<T, InputError> {
    T::deserialize(StrDeserializer::<InputError>::new(tag)).map_err(|_| invalid())
}

impl<'a> Input<'a, '_> {
    pub fn texts(&mut self, token: JsonToken<'a>) -> Result<Produced<Vec<String>>, InputError> {
        self.array(token, |input, token| input.text(token))
    }

    pub fn optional_text(
        &mut self,
        token: JsonToken<'a>,
    ) -> Result<Produced<Option<String>>, InputError> {
        self.maybe(token, |input, token| input.text(token))
    }

    pub fn scalar<T: Copy>(&self, value: T) -> Result<Produced<T>, InputError> {
        Ok(self
            .control
            .finish(value, self.control.empty_reservation())?)
    }

    pub fn maybe<T>(
        &mut self,
        token: JsonToken<'a>,
        read: impl FnOnce(&mut Self, JsonToken<'a>) -> Result<Produced<T>, InputError>,
    ) -> Result<Produced<Option<T>>, InputError> {
        if token == JsonToken::Null {
            return Ok(self
                .control
                .finish(None, self.control.empty_reservation())?);
        }
        let (value, memory) = read(self, token)?.into_parts();
        Ok(self.control.finish(Some(value), memory)?)
    }

    pub fn boxed<T>(
        &mut self,
        token: JsonToken<'a>,
        read: impl FnOnce(&mut Self, JsonToken<'a>) -> Result<Produced<T>, InputError>,
    ) -> Result<Produced<Box<T>>, InputError> {
        let value = read(self, token)?;
        let box_memory = self.control.reserve(size_of::<T>())?;
        let (value, memory) = value.into_parts();
        Ok(self
            .control
            .finish(Box::new(value), self.control.combine(memory, box_memory))?)
    }

    pub fn array<T>(
        &mut self,
        first: JsonToken<'a>,
        mut read: impl FnMut(&mut Self, JsonToken<'a>) -> Result<Produced<T>, InputError>,
    ) -> Result<Produced<Vec<T>>, InputError> {
        let mut values = ProductionVec::new(self.control);
        self.visit_array(first, |input, token| {
            values.push_produced(read(input, token)?)?;
            Ok(())
        })?;
        Ok(values.finish()?)
    }

    /// Validate and visit each original array item without constructing a second collection.
    pub fn visit_array(
        &mut self,
        first: JsonToken<'a>,
        mut visit: impl FnMut(&mut Self, JsonToken<'a>) -> Result<(), InputError>,
    ) -> Result<(), InputError> {
        let mut cursor = self.array_cursor(first)?;
        while let Some(token) = cursor.next(self)? {
            visit(self, token)?;
        }
        Ok(())
    }

    pub fn set<T: Ord>(
        &mut self,
        first: JsonToken<'a>,
        mut read: impl FnMut(&mut Self, JsonToken<'a>) -> Result<T, InputError>,
    ) -> Result<Produced<uqa_core::memory::OwnedSet<T>>, InputError> {
        if first != JsonToken::StartArray {
            return Err(invalid());
        }
        self.enter()?;
        let mut memory = self.control.empty_reservation();
        let mut values = uqa_core::memory::OwnedSet::new();
        loop {
            let token = self.next()?;
            if token == JsonToken::EndArray {
                break;
            }
            let value = read(self, token)?;
            if !values.contains(&value) {
                let entry = self
                    .control
                    .reserve(uqa_core::memory::OwnedSet::<T>::entry_bytes())?;
                memory = self.control.combine(memory, entry);
                values.insert(value);
            }
        }
        self.remaining_depth += 1;
        Ok(self.control.finish(values, memory)?)
    }

    pub fn identity(&mut self, first: JsonToken<'a>) -> Result<[u8; 16], InputError> {
        if first != JsonToken::StartArray {
            return Err(invalid());
        }
        self.enter()?;
        let mut value = [0; 16];
        for byte in &mut value {
            *byte = number(self.next()?)?;
        }
        if self.next()? != JsonToken::EndArray {
            return Err(invalid());
        }
        self.remaining_depth += 1;
        Ok(value)
    }
}
