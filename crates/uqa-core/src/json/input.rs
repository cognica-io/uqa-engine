//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Typed input failures retain native diagnostics and original resource owners.

use super::{
    decode_json_string_with_work, JsonPosition, JsonReadError, JsonReadWork, JsonReader,
    JsonSyntaxError, JsonToken,
};
use crate::{
    image_io::{AsyncImageInput, ImageIoError},
    memory::{MemoryError, Produced, ProductionControl},
    QueryCancelled,
};

mod fields;
mod projected;

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
    pub(super) fn retain_syntax(self, reader: &JsonReader<'_, '_>) -> Self {
        match self {
            Self::Invalid => Self::from_reader(reader, JsonReadError::InvalidJson),
            error => error,
        }
    }

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
    WideSigned(i128),
    WideUnsigned(u128),
    Float(f64),
    Number,
    String(Produced<String>),
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
            Self::WideSigned(value) => write!(f, "integer `{value}` as i128"),
            Self::WideUnsigned(value) => write!(f, "integer `{value}` as u128"),
            Self::Float(value) => {
                f.write_str("floating point `")?;
                serde_json::to_writer(JsonFloatWriter(f), value).map_err(|_| std::fmt::Error)?;
                f.write_str("`")
            }
            Self::Number => f.write_str("number"),
            Self::String(value) => write!(f, "string {:?}", value.as_str()),
            Self::Sequence => f.write_str("sequence"),
            Self::Map => f.write_str("map"),
            Self::Type(name) => f.write_str(name),
        }
    }
}

struct JsonFloatWriter<'a, 'b>(&'a mut std::fmt::Formatter<'b>);

impl std::io::Write for JsonFloatWriter<'_, '_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let text = std::str::from_utf8(bytes).map_err(|_| std::io::ErrorKind::InvalidData)?;
        self.0
            .write_str(text)
            .map_err(|_| std::io::ErrorKind::Other)?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl JsonReader<'_, '_> {
    pub(super) async fn type_error_with_work<W: JsonReadWork>(
        &mut self,
        expected: &'static str,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<JsonDiagnostic, W::Error> {
        let actual = match self.peek() {
            Some(b'[') => Unexpected::Sequence,
            Some(b'{') => Unexpected::Map,
            _ => {
                let event = self.value(work, false).await?;
                return JsonDiagnostic::observe_type_with_work(
                    event.token,
                    self.position(),
                    expected,
                    control,
                    work,
                )
                .await;
            }
        };
        Ok(JsonDiagnostic {
            position: self.position(),
            cause: Cause::Type { expected, actual },
        })
    }
}

#[derive(Debug)]
enum Cause {
    Syntax(super::JsonSyntaxKind),
    DataSyntax(super::JsonSyntaxKind),
    Message {
        prefix: &'static str,
        message: &'static str,
    },
    RetainedMessage {
        format: &'static JsonMessageFormat,
        text: Option<Produced<String>>,
    },
    Field(fields::FieldFailure),
    Type {
        expected: &'static str,
        actual: Unexpected,
    },
    Value {
        expected: &'static str,
        actual: Unexpected,
    },
}

/// Static framing for one admitted diagnostic payload, including its complete redacted message.
#[derive(Debug)]
pub struct JsonMessageFormat {
    prefix: &'static str,
    suffix: &'static str,
    redacted: &'static str,
}

impl JsonMessageFormat {
    pub const fn new(prefix: &'static str, suffix: &'static str, redacted: &'static str) -> Self {
        Self {
            prefix,
            suffix,
            redacted,
        }
    }
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
    pub(super) fn invalid_unsigned(
        value: u64,
        expected: &'static str,
        position: JsonPosition,
    ) -> Self {
        Self {
            position,
            cause: Cause::Value {
                expected,
                actual: Unexpected::Unsigned(value),
            },
        }
    }

    pub(super) fn invalid_signed(
        value: i64,
        expected: &'static str,
        position: JsonPosition,
    ) -> Self {
        Self {
            position,
            cause: Cause::Value {
                expected,
                actual: Unexpected::Signed(value),
            },
        }
    }

    pub(super) fn invalid_float(
        value: f64,
        expected: &'static str,
        position: JsonPosition,
    ) -> Self {
        Self {
            position,
            cause: Cause::Type {
                expected,
                actual: Unexpected::Float(value),
            },
        }
    }

    pub fn message(message: &'static str, position: JsonPosition) -> Self {
        Self::prefixed_message("", message, position)
    }

    /// Preserve a caller-owned static diagnostic without allocating or including source payloads.
    pub fn prefixed_message(
        prefix: &'static str,
        message: &'static str,
        position: JsonPosition,
    ) -> Self {
        Self {
            position,
            cause: Cause::Message { prefix, message },
        }
    }

    /// Retain an admitted dynamic diagnostic until the owning boundary releases or redacts it. Static punctuation and a complete redacted message avoid a second formatted allocation.
    pub fn retained_message(
        format: &'static JsonMessageFormat,
        text: Produced<String>,
        position: JsonPosition,
    ) -> Self {
        Self {
            position,
            cause: Cause::RetainedMessage {
                format,
                text: Some(text),
            },
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
        position: JsonPosition,
        expected: &'static str,
        input: &mut AsyncImageInput<'_>,
    ) -> Result<Self, JsonInputError> {
        let control = ProductionControl::new(input.memory, input.cancellation, input.cancellation);
        Self::invalid_type_with_work(token, position, expected, &control, input).await
    }

    /// Observe the same direct-deserializer failure with the caller's original production ownership and work scheduler.
    pub async fn invalid_type_with_work<W: JsonReadWork<Error = JsonReadError>>(
        token: JsonToken<'_, &'_ [u8]>,
        position: JsonPosition,
        expected: &'static str,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<Self, JsonInputError> {
        Self::observe_type_with_work(token, position, expected, control, work)
            .await
            .map_err(Into::into)
    }

    /// Preserve a direct typed observation through a caller-owned error channel. String values retain either their original admitted lease or the ordinary producer's ownership.
    pub async fn observe_type_with_work<W: JsonReadWork>(
        token: JsonToken<'_, &'_ [u8]>,
        mut position: JsonPosition,
        expected: &'static str,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<Self, W::Error> {
        let actual = match token {
            JsonToken::Null => Unexpected::Null,
            JsonToken::Bool(value) => Unexpected::Bool(value),
            JsonToken::Number(bytes) => number(bytes),
            JsonToken::String(bytes) | JsonToken::Key(bytes) => {
                Unexpected::String(decode_json_string_with_work(bytes, control, work).await?)
            }
            JsonToken::StartArray => Unexpected::Sequence,
            JsonToken::StartObject => Unexpected::Map,
            JsonToken::EndArray | JsonToken::EndObject => {
                return Err(JsonReadError::InvalidJson.into())
            }
        };
        if matches!(actual, Unexpected::Sequence | Unexpected::Map) {
            position.byte_offset = position
                .byte_offset
                .checked_sub(1)
                .ok_or(JsonReadError::InvalidJson)?;
            position.column = position
                .column
                .checked_sub(1)
                .ok_or(JsonReadError::InvalidJson)?;
        }
        Ok(Self {
            position,
            cause: Cause::Type { expected, actual },
        })
    }

    /// A string-encoded scalar reports its inner parse failure as data while retaining that parse's own location and message.
    pub fn into_data(mut self) -> Self {
        if let Cause::Syntax(kind) = self.cause {
            self.cause = Cause::DataSyntax(kind);
        }
        self
    }

    /// Drop original scalar values before crossing a boundary that must not disclose input payloads. Syntax, expected type, error category and source position remain available.
    pub fn into_type_only(mut self) -> Self {
        if let Cause::RetainedMessage { text, .. } = &mut self.cause {
            *text = None;
        }
        if let Cause::Field(failure) = &mut self.cause {
            failure.redact();
        }
        if let Cause::Type { actual, .. } | Cause::Value { actual, .. } = &mut self.cause {
            match actual {
                Unexpected::Bool(_) => *actual = Unexpected::Type("boolean"),
                Unexpected::Signed(_) | Unexpected::Unsigned(_) => {
                    *actual = Unexpected::Type("integer");
                }
                Unexpected::WideSigned(_) => *actual = Unexpected::Type("integer as i128"),
                Unexpected::WideUnsigned(_) => *actual = Unexpected::Type("integer as u128"),
                Unexpected::String(_) => *actual = Unexpected::Type("string"),
                Unexpected::Float(_) => *actual = Unexpected::Type("floating point"),
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
            Cause::Syntax(kind) | Cause::DataSyntax(kind) => f.write_str(kind.message())?,
            Cause::Message { prefix, message } => {
                f.write_str(prefix)?;
                f.write_str(message)?;
            }
            Cause::RetainedMessage { format, text } => {
                if let Some(text) = text {
                    f.write_str(format.prefix)?;
                    f.write_str(text)?;
                    f.write_str(format.suffix)?;
                } else {
                    f.write_str(format.redacted)?;
                }
            }
            Cause::Field(failure) => std::fmt::Display::fmt(failure, f)?,
            Cause::Type { expected, actual } => {
                write!(f, "invalid type: {actual}, expected {expected}")?;
            }
            Cause::Value { expected, actual } => {
                write!(f, "invalid value: {actual}, expected {expected}")?;
            }
        }
        if self.position.line == 0 {
            Ok(())
        } else {
            write!(
                f,
                " at line {} column {}",
                self.position.line, self.position.column
            )
        }
    }
}

impl std::error::Error for JsonDiagnostic {}
