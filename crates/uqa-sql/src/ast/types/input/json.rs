//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored type fields consume one admitted JSON cursor in their original order.

use crate::SQLError;
use uqa_core::{
    json::{decode_json_string_with_control, JsonReadError, JsonReader, JsonToken},
    memory::{Produced, ProductionControl},
};

pub(super) struct Input<'a, 'c> {
    reader: JsonReader<'a, 'c>,
    pub control: ProductionControl<'c>,
    remaining_depth: usize,
}

pub(super) fn invalid() -> SQLError {
    SQLError::Internal("invalid stored column type JSON".into())
}

fn error(error: JsonReadError) -> SQLError {
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
            control,
            remaining_depth: 128,
        }
    }

    pub fn next(&mut self) -> Result<JsonToken<'a>, SQLError> {
        self.reader
            .next_event()
            .map_err(error)?
            .map(|event| event.token)
            .ok_or_else(invalid)
    }

    pub fn end(&mut self) -> Result<(), SQLError> {
        if self.reader.next_event().map_err(error)?.is_some() {
            return Err(invalid());
        }
        self.control.check()?;
        Ok(())
    }

    pub fn text(&self, token: JsonToken<'_>) -> Result<Produced<String>, SQLError> {
        let (JsonToken::String(value) | JsonToken::Key(value)) = token else {
            return Err(invalid());
        };
        decode_json_string_with_control(value, &self.control).map_err(error)
    }

    pub fn tag(&mut self, first: JsonToken<'a>) -> Result<(Produced<String>, bool), SQLError> {
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

    pub fn close_tag(&mut self, object: bool) -> Result<(), SQLError> {
        if object {
            if self.next()? != JsonToken::EndObject {
                return Err(invalid());
            }
            self.remaining_depth += 1;
        }
        Ok(())
    }

    pub fn unit_tag(&mut self, first: JsonToken<'a>) -> Result<Produced<String>, SQLError> {
        let (tag, object) = self.tag(first)?;
        if object && self.next()? != JsonToken::Null {
            return Err(invalid());
        }
        self.close_tag(object)?;
        Ok(tag)
    }

    /// Struct maps ignore unknown values; sequence forms retain every declared positional field.
    pub fn fields(
        &mut self,
        first: JsonToken<'a>,
        names: &[&str],
        required: u64,
        mut field: impl FnMut(&mut Self, usize, JsonToken<'a>) -> Result<(), SQLError>,
    ) -> Result<(), SQLError> {
        self.enter()?;
        let mut seen = 0_u64;
        match first {
            JsonToken::StartObject => loop {
                let token = self.next()?;
                if token == JsonToken::EndObject {
                    break;
                }
                let JsonToken::Key(_) = token else {
                    return Err(invalid());
                };
                let name = self.text(token)?;
                if let Some(index) = names
                    .iter()
                    .position(|candidate| *candidate == name.as_str())
                {
                    let bit = 1 << index;
                    if seen & bit != 0 {
                        return Err(invalid());
                    }
                    seen |= bit;
                    let token = self.next()?;
                    field(self, index, token)?;
                } else {
                    let token = self.next()?;
                    self.skip(token)?;
                }
            },
            JsonToken::StartArray => {
                let mut index = 0;
                loop {
                    let token = self.next()?;
                    if token == JsonToken::EndArray {
                        if index != names.len() {
                            return Err(invalid());
                        }
                        break;
                    }
                    if index == names.len() {
                        return Err(invalid());
                    }
                    field(self, index, token)?;
                    seen |= 1 << index;
                    index += 1;
                }
            }
            _ => return Err(invalid()),
        }
        if seen & required != required {
            return Err(invalid());
        }
        self.remaining_depth += 1;
        Ok(())
    }

    /// Serde's recognized containers consume recursion depth; ignored values do not.
    fn enter(&mut self) -> Result<(), SQLError> {
        self.remaining_depth -= 1;
        if self.remaining_depth == 0 {
            return Err(invalid());
        }
        Ok(())
    }

    fn skip(&mut self, first: JsonToken<'a>) -> Result<(), SQLError> {
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

pub(super) trait Integer: serde::de::DeserializeOwned {
    const MAX_LITERAL_BYTES: usize;
}

impl Integer for u32 {
    const MAX_LITERAL_BYTES: usize = 10;
}

impl Integer for i32 {
    const MAX_LITERAL_BYTES: usize = 11;
}

pub(super) fn number<T: Integer>(token: JsonToken<'_>) -> Result<T, SQLError> {
    let JsonToken::Number(text) = token else {
        return Err(invalid());
    };
    if text.len() > T::MAX_LITERAL_BYTES {
        return Err(invalid());
    }
    // The public scalar parser visits at most eleven bytes and constructs no payload owner.
    serde_json::from_str(text).map_err(|_| invalid())
}

pub(super) fn optional<T: Integer>(token: JsonToken<'_>) -> Result<Option<T>, SQLError> {
    if token == JsonToken::Null {
        Ok(None)
    } else {
        number(token).map(Some)
    }
}
