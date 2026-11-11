//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored SQL declarations consume one admitted JSON cursor in their original order.

mod error;
pub(crate) use error::InputError;

mod fields;
mod objects;
mod parts;
mod values;
pub(crate) use parts::Parts;
use uqa_core::{
    json::{decode_json_string_with_control, JsonReadError, JsonReader, JsonToken},
    memory::{Produced, ProductionControl},
};
pub(crate) use values::{boolean, unit};

pub(crate) struct Input<'a, 'c> {
    reader: JsonReader<'a, 'c>,
    text: &'a str,
    range: std::ops::Range<usize>,
    content_numbers: bool,
    pub control: ProductionControl<'c>,
    remaining_depth: usize,
}

pub(crate) fn invalid() -> InputError {
    InputError::Invalid
}

fn error(error: JsonReadError) -> InputError {
    match error {
        JsonReadError::Memory(error) => error.into(),
        JsonReadError::Cancelled(error) => error.into(),
        JsonReadError::InvalidJson | JsonReadError::Malformed { .. } => invalid(),
    }
}

impl<'a, 'c> Input<'a, 'c> {
    pub fn new(text: &'a str, control: ProductionControl<'c>) -> Self {
        Self {
            reader: JsonReader::with_control(text, &control).with_ignored_string_escapes(),
            text,
            range: 0..0,
            content_numbers: false,
            control,
            remaining_depth: 128,
        }
    }

    /// Flattened fields are typed from Serde's generic content, whose signed integer zero has already lost its lexical minus sign.
    pub fn from_content(text: &'a str, control: ProductionControl<'c>) -> Self {
        let mut input = Self::new(text, control);
        input.content_numbers = true;
        input
    }

    pub fn next(&mut self) -> Result<JsonToken<'a>, InputError> {
        let event = self
            .reader
            .next_event()
            .map_err(error)?
            .ok_or_else(invalid)?;
        self.range = event.range;
        Ok(
            if self.content_numbers && event.token == JsonToken::Number("-0") {
                JsonToken::Number("0")
            } else {
                event.token
            },
        )
    }

    pub fn end(&mut self) -> Result<(), InputError> {
        if self.reader.next_event().map_err(error)?.is_some() {
            return Err(invalid());
        }
        self.control.check()?;
        Ok(())
    }

    pub fn text(&self, token: JsonToken<'_>) -> Result<Produced<String>, InputError> {
        let (JsonToken::String(value) | JsonToken::Key(value)) = token else {
            return Err(invalid());
        };
        decode_json_string_with_control(value, &self.control).map_err(error)
    }

    pub fn tag(&mut self, first: JsonToken<'a>) -> Result<(Produced<String>, bool), InputError> {
        match first {
            JsonToken::String(_) => Ok((self.text(first)?, false)),
            JsonToken::StartObject => {
                self.enter()?;
                let token = self.next()?;
                if !matches!(token, JsonToken::Key(_)) {
                    return Err(invalid());
                }
                Ok((self.text(token)?, true))
            }
            _ => Err(invalid()),
        }
    }

    pub fn close_tag(&mut self, object: bool) -> Result<(), InputError> {
        if object {
            if self.next()? != JsonToken::EndObject {
                return Err(invalid());
            }
            self.remaining_depth += 1;
        }
        Ok(())
    }

    pub fn unit_tag(&mut self, first: JsonToken<'a>) -> Result<Produced<String>, InputError> {
        let (tag, object) = self.tag(first)?;
        if object && self.next()? != JsonToken::Null {
            return Err(invalid());
        }
        self.close_tag(object)?;
        Ok(tag)
    }

    /// Serde's recognized containers consume recursion depth; ignored values do not.
    fn enter(&mut self) -> Result<(), InputError> {
        self.remaining_depth -= 1;
        if self.remaining_depth == 0 {
            return Err(invalid());
        }
        Ok(())
    }

    fn skip(&mut self, first: JsonToken<'a>) -> Result<(), InputError> {
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

pub(crate) trait Integer: std::str::FromStr {
    const MAX_LITERAL_BYTES: usize;
}

impl Integer for u32 {
    const MAX_LITERAL_BYTES: usize = 10;
}

impl Integer for i32 {
    const MAX_LITERAL_BYTES: usize = 11;
}

pub(crate) fn number<T: Integer>(token: JsonToken<'_>) -> Result<T, InputError> {
    let JsonToken::Number(text) = token else {
        return Err(invalid());
    };
    if text.len() > T::MAX_LITERAL_BYTES || text == "-0" || text.contains(['.', 'e', 'E']) {
        return Err(invalid());
    }
    // The JSON cursor validated the lexical token; typed integers reject fractional/exponent forms and lexical negative zero before allocation-free conversion.
    text.parse().map_err(|_| invalid())
}

pub(crate) fn optional<T: Integer>(token: JsonToken<'_>) -> Result<Option<T>, InputError> {
    if token == JsonToken::Null {
        Ok(None)
    } else {
        number(token).map(Some)
    }
}
