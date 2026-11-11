//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored text and vector field declarations retain their original ordered buffers.

mod row;
pub use row::row;

use crate::{
    read_control::StorageReadControl, StorageBackendError, StorageBackendResult, VectorFieldSchema,
};
use serde::de::Error as _;
use uqa_core::{
    json::{decode_json_string, JsonReadError, JsonReader, JsonToken},
    memory::{Budgeted, BudgetedVec},
};

/// Decode the stored full-text field sequence. Ordering, repeated names and exact decoded spelling are preserved; schema validation remains with the catalog consumer.
pub fn field_names(
    bytes: &[u8],
    control: &StorageReadControl,
) -> StorageBackendResult<Budgeted<Vec<String>>> {
    Input::new(bytes, control).sequence(true, |input, token| input.text(token))
}

/// Decode stored vector field declarations without copying their enclosing JSON or collecting an intermediate value tree. Every destination buffer is admitted before construction.
pub fn vector_fields(
    bytes: &[u8],
    control: &StorageReadControl,
) -> StorageBackendResult<Budgeted<Vec<VectorFieldSchema>>> {
    Input::new(bytes, control).sequence(true, Input::vector)
}

struct Input<'a, 'c> {
    reader: JsonReader<'a, 'c>,
    control: &'c StorageReadControl,
}

impl<'a, 'c> Input<'a, 'c> {
    fn new(bytes: &'a [u8], control: &'c StorageReadControl) -> Self {
        Self {
            reader: JsonReader::from_slice(bytes, control.memory(), control.cancellation())
                .with_ignored_string_escapes(),
            control,
        }
    }

    fn next(&mut self) -> StorageBackendResult<JsonToken<'a>> {
        self.reader
            .next_event()
            .map_err(json_error)?
            .map(|event| event.token)
            .ok_or_else(|| invalid("unexpected end of table field JSON"))
    }

    fn text(&self, token: JsonToken<'_>) -> StorageBackendResult<Budgeted<String>> {
        let JsonToken::String(value) = token else {
            return Err(invalid("expected a table field name"));
        };
        decode_json_string(value, self.control.memory(), self.control.cancellation())
            .map_err(json_error)
    }

    fn sequence<T>(
        mut self,
        require_end: bool,
        mut element: impl FnMut(&mut Self, JsonToken<'a>) -> StorageBackendResult<Budgeted<T>>,
    ) -> StorageBackendResult<Budgeted<Vec<T>>> {
        self.control.check()?;
        if self.next()? != JsonToken::StartArray {
            return Err(invalid("expected a table field array"));
        }
        let mut payloads = self.control.memory().empty_reservation();
        let mut values = BudgetedVec::new(self.control.memory());
        loop {
            let token = self.next()?;
            if token == JsonToken::EndArray {
                break;
            }
            let value = element(&mut self, token)?;
            values.reserve(1)?;
            let (value, memory) = value.into_parts();
            payloads.absorb(memory);
            values.push(value)?;
        }
        if require_end && self.reader.next_event().map_err(json_error)?.is_some() {
            return Err(invalid("trailing table field JSON"));
        }
        self.control.check()?;
        let (values, memory) = values.into_parts();
        payloads.absorb(memory);
        Ok(Budgeted::new(values, payloads))
    }

    fn vector(
        &mut self,
        first: JsonToken<'a>,
    ) -> StorageBackendResult<Budgeted<VectorFieldSchema>> {
        let object = match first {
            JsonToken::StartObject => true,
            JsonToken::StartArray => false,
            _ => return Err(invalid("expected a vector field declaration")),
        };
        let mut name = None;
        let mut dimensions = None;
        let mut position = 0;
        loop {
            let token = self.next()?;
            if matches!(
                (object, token),
                (true, JsonToken::EndObject) | (false, JsonToken::EndArray)
            ) {
                break;
            }
            let selected = if object {
                let JsonToken::Key(key) = token else {
                    return Err(invalid("expected a vector field property"));
                };
                let key =
                    decode_json_string(key, self.control.memory(), self.control.cancellation())
                        .map_err(json_error)?;
                ["field", "dimensions"]
                    .iter()
                    .position(|field| *field == key.as_str())
            } else {
                if position == 2 {
                    return Err(invalid("too many positional vector field properties"));
                }
                position += 1;
                Some(position - 1)
            };
            if selected == Some(0) && name.is_some() {
                return Err(invalid("duplicate field `field`"));
            }
            if selected == Some(1) && dimensions.is_some() {
                return Err(invalid("duplicate field `dimensions`"));
            }
            let value = if object { self.next()? } else { token };
            match selected {
                Some(0) => name = Some(self.text(value)?),
                Some(1) => {
                    let JsonToken::Number(value) = value else {
                        return Err(invalid("expected vector dimensions as an integer"));
                    };
                    // The public scalar decoder preserves the stored u32 classification, including negative zero and exponent forms.
                    dimensions = Some(serde_json::from_str::<u32>(value)?);
                }
                None => self.skip(value)?,
                _ => unreachable!(),
            }
        }
        let name = name.ok_or_else(|| invalid("missing field `field`"))?;
        let dimensions = dimensions.ok_or_else(|| invalid("missing field `dimensions`"))?;
        let (field, memory) = name.into_parts();
        Ok(Budgeted::new(
            VectorFieldSchema { field, dimensions },
            memory,
        ))
    }

    fn skip(&mut self, first: JsonToken<'_>) -> StorageBackendResult<()> {
        let mut depth = usize::from(matches!(
            first,
            JsonToken::StartArray | JsonToken::StartObject
        ));
        while depth != 0 {
            match self.next()? {
                JsonToken::StartArray | JsonToken::StartObject => depth += 1,
                JsonToken::EndArray | JsonToken::EndObject => depth -= 1,
                _ => (),
            }
        }
        Ok(())
    }
}

fn invalid(message: impl std::fmt::Display) -> StorageBackendError {
    StorageBackendError::Serde(serde_json::Error::custom(message))
}

fn json_error(error: JsonReadError) -> StorageBackendError {
    match error {
        JsonReadError::Memory(error) => error.into(),
        JsonReadError::Cancelled(error) => error.into(),
        error => invalid(error),
    }
}

#[cfg(test)]
mod tests;
