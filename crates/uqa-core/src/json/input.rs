//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Typed input failures retain native diagnostics and original resource owners.

use super::{
    decode_json_string_from, JsonPosition, JsonReadError, JsonReader, JsonSyntaxError, JsonToken,
};
use crate::{
    image_io::{AsyncImageInput, ImageIoError},
    memory::{Budgeted, MemoryError},
    QueryCancelled,
};

#[derive(Debug, thiserror::Error)]
pub enum JsonInputError {
    #[error(transparent)]
    Diagnostic(#[from] JsonDiagnostic),
    #[error(transparent)]
    Memory(#[from] MemoryError),
    #[error(transparent)]
    Cancelled(#[from] QueryCancelled),
    #[error(transparent)]
    Image(ImageIoError),
    #[error("invalid JSON input")]
    Invalid,
}

impl JsonInputError {
    pub fn from_reader(reader: &JsonReader<'_, '_>, error: JsonReadError) -> Self {
        match error {
            JsonReadError::InvalidJson => reader
                .syntax_error()
                .map_or(Self::Invalid, |error| Self::Diagnostic(error.into())),
            error => error.into(),
        }
    }
}

impl From<JsonReadError> for JsonInputError {
    fn from(error: JsonReadError) -> Self {
        match error {
            JsonReadError::Memory(error) => Self::Memory(error),
            JsonReadError::Cancelled(error) => Self::Cancelled(error),
            JsonReadError::InvalidJson | JsonReadError::Malformed { .. } => Self::Invalid,
        }
    }
}

impl From<ImageIoError> for JsonInputError {
    fn from(error: ImageIoError) -> Self {
        match error {
            ImageIoError::Memory(error) => Self::Memory(error),
            ImageIoError::Cancelled(error) => Self::Cancelled(error),
            error => Self::Image(error),
        }
    }
}

#[derive(Debug)]
enum Unexpected {
    Null,
    Bool(bool),
    Signed(i64),
    Unsigned(u64),
    Number,
    String(Budgeted<String>),
    Sequence,
    Map,
    Type(&'static str),
}

impl std::fmt::Display for Unexpected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Null => f.write_str("null"),
            Self::Bool(value) => write!(f, "boolean `{value}`"),
            Self::Signed(value) => write!(f, "integer `{value}`"),
            Self::Unsigned(value) => write!(f, "integer `{value}`"),
            Self::Number => f.write_str("number"),
            Self::String(value) => write!(f, "string {:?}", value.as_str()),
            Self::Sequence => f.write_str("sequence"),
            Self::Map => f.write_str("map"),
            Self::Type(name) => f.write_str(name),
        }
    }
}

#[derive(Debug)]
enum Cause {
    Syntax(super::JsonSyntaxKind),
    Message(&'static str),
    Type {
        expected: &'static str,
        actual: Unexpected,
    },
}

/// A diagnostic owns only values needed for its original message. Sensitive image boundaries can retain the type and position while retiring those values.
#[derive(Debug)]
pub struct JsonDiagnostic {
    position: JsonPosition,
    cause: Cause,
}

impl From<JsonSyntaxError> for JsonDiagnostic {
    fn from(error: JsonSyntaxError) -> Self {
        Self {
            position: error.position,
            cause: Cause::Syntax(error.kind),
        }
    }
}

impl JsonDiagnostic {
    pub fn message(message: &'static str, position: JsonPosition) -> Self {
        Self {
            position,
            cause: Cause::Message(message),
        }
    }

    pub fn position(&self) -> JsonPosition {
        self.position
    }

    pub fn is_eof(&self) -> bool {
        matches!(self.cause, Cause::Syntax(kind) if kind.is_eof())
    }

    pub fn is_syntax(&self) -> bool {
        matches!(self.cause, Cause::Syntax(kind) if !kind.is_eof())
    }

    pub fn is_data(&self) -> bool {
        !matches!(self.cause, Cause::Syntax(_))
    }

    /// Observe a validated token at its original direct-deserializer boundary. Containers fail before their opening delimiter; native scalars fail after their complete token.
    pub async fn invalid_type_from(
        token: JsonToken<'_, &'_ [u8]>,
        mut position: JsonPosition,
        expected: &'static str,
        input: &mut AsyncImageInput<'_>,
    ) -> Result<Self, JsonInputError> {
        let actual = match token {
            JsonToken::Null => Unexpected::Null,
            JsonToken::Bool(value) => Unexpected::Bool(value),
            JsonToken::Number(bytes) => number(bytes),
            JsonToken::String(bytes) | JsonToken::Key(bytes) => {
                Unexpected::String(decode_json_string_from(bytes, input).await?)
            }
            JsonToken::StartArray => Unexpected::Sequence,
            JsonToken::StartObject => Unexpected::Map,
            JsonToken::EndArray | JsonToken::EndObject => return Err(JsonInputError::Invalid),
        };
        if matches!(actual, Unexpected::Sequence | Unexpected::Map) {
            position.byte_offset = position
                .byte_offset
                .checked_sub(1)
                .ok_or(JsonInputError::Invalid)?;
            position.column = position
                .column
                .checked_sub(1)
                .ok_or(JsonInputError::Invalid)?;
        }
        Ok(Self {
            position,
            cause: Cause::Type { expected, actual },
        })
    }

    /// Drop original scalar values before crossing a boundary that must not disclose input payloads. Syntax, expected type, error category and source position remain available.
    pub fn into_type_only(mut self) -> Self {
        if let Cause::Type { actual, .. } = &mut self.cause {
            match actual {
                Unexpected::Bool(_) => *actual = Unexpected::Type("boolean"),
                Unexpected::Signed(_) | Unexpected::Unsigned(_) => {
                    *actual = Unexpected::Type("integer");
                }
                Unexpected::String(_) => *actual = Unexpected::Type("string"),
                _ => (),
            }
        }
        self
    }
}

fn number(bytes: &[u8]) -> Unexpected {
    if bytes.len() <= 20 {
        if let Ok(text) = std::str::from_utf8(bytes) {
            if text.starts_with('-') {
                if let Ok(value) = text.parse::<i64>() {
                    return Unexpected::Signed(value);
                }
            } else if let Ok(value) = text.parse::<u64>() {
                return Unexpected::Unsigned(value);
            }
        }
    }
    Unexpected::Number
}

impl std::fmt::Display for JsonDiagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.cause {
            Cause::Syntax(kind) => f.write_str(kind.message())?,
            Cause::Message(message) => f.write_str(message)?,
            Cause::Type { expected, actual } => {
                write!(f, "invalid type: {actual}, expected {expected}")?;
            }
        }
        write!(
            f,
            " at line {} column {}",
            self.position.line, self.position.column
        )
    }
}

impl std::error::Error for JsonDiagnostic {}
