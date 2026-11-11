//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Construct native JSON values through the shared retained grammar and operation schedule.

use crate::work::Work;
use serde::de::Error as _;
use serde_json::Value;
use uqa_core::{
    image_io::ImageIoError,
    json::{
        decode_json_string_with_work, JsonExpected, JsonReadError, JsonReadWork, JsonReader,
        JsonToken,
    },
    memory::ProductionControl,
};

mod comparison;
pub(crate) use comparison::{bytes_compare, bytes_equal};

mod output;
pub(crate) use output::{write_raw, write_string, write_value, Output};

pub(crate) fn fragment(text: &str, position: usize) -> &str {
    let mut end = position.saturating_add(4096).min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[position..end]
}

enum Frame {
    Array(Vec<Value>),
    Object(serde_json::Map<String, Value>, Option<String>),
}

enum Error<E> {
    Grammar(JsonReadError),
    Work(E),
}

impl<E> From<JsonReadError> for Error<E> {
    fn from(error: JsonReadError) -> Self {
        Self::Grammar(error)
    }
}

struct Scheduler<'a, W, E>(&'a mut W, std::marker::PhantomData<fn() -> E>);

impl<W: Work<E>, E> JsonReadWork for Scheduler<'_, W, E> {
    type Error = Error<E>;

    async fn checkpoint(&mut self) -> Result<(), Error<E>> {
        self.0.checkpoint().await.map_err(Error::Work)
    }
}

pub(crate) trait InputError: From<serde_json::Error> + From<ImageIoError> + Send {}
impl<E: From<serde_json::Error> + From<ImageIoError> + Send> InputError for E {}

/// Borrowed syntax retains the enclosing schedule; Analysis owns decoded model allocations.
pub(crate) struct Input<'a> {
    reader: JsonReader<'a, 'static>,
    bytes: &'a [u8],
    pending: Option<JsonToken<'a, &'a [u8]>>,
}

impl<'a> Input<'a> {
    pub(crate) fn new(bytes: &'a [u8]) -> Self {
        Self {
            reader: JsonReader::from_slice_with_control(bytes, &ProductionControl::uncontrolled())
                .with_depth_limit(127),
            bytes,
            pending: None,
        }
    }

    pub(crate) async fn next<E: InputError>(
        &mut self,
        work: &mut impl Work<E>,
    ) -> Result<Option<JsonToken<'a, &'a [u8]>>, E> {
        if let Some(token) = self.pending.take() {
            return Ok(Some(token));
        }
        match self
            .reader
            .next_event_with_work(&mut Scheduler(work, std::marker::PhantomData))
            .await
        {
            Ok(event) => Ok(event.map(|event| event.token)),
            Err(error) => {
                Err(diagnostic(error, self.bytes, self.reader.syntax_position(), work).await?)
            }
        }
    }

    pub(crate) async fn peek<E: InputError>(
        &mut self,
        work: &mut impl Work<E>,
    ) -> Result<Option<JsonToken<'a, &'a [u8]>>, E> {
        if self.pending.is_none() {
            self.pending = self.next(work).await?;
        }
        Ok(self.pending)
    }

    pub(crate) async fn located<E: InputError>(
        &self,
        error: serde_json::Error,
        work: &mut impl Work<E>,
    ) -> Result<E, E> {
        let (position, _) = self.reader.syntax_position();
        let mut line = 1;
        let mut column = 0;
        for part in self.bytes[..position].chunks(4096) {
            work.checkpoint().await?;
            for byte in part {
                if *byte == b'\n' {
                    line += 1;
                    column = 0;
                } else {
                    column += 1;
                }
            }
        }
        Ok(serde_json::Error::custom(format!("{error} at line {line} column {column}")).into())
    }

    pub(crate) async fn string<E: InputError>(
        &mut self,
        encoded: &[u8],
        work: &mut impl Work<E>,
    ) -> Result<String, E> {
        string(encoded, work).await
    }

    #[cfg(any(feature = "nori", feature = "kuromoji"))]
    pub(crate) async fn root<E: InputError>(
        &mut self,
        work: &mut impl Work<E>,
    ) -> Result<Value, E> {
        let value = self.value(work).await?;
        if self.next(work).await?.is_some() {
            unreachable!("the grammar rejects multiple root values");
        }
        Ok(value)
    }

    pub(crate) async fn value<E: InputError>(
        &mut self,
        work: &mut impl Work<E>,
    ) -> Result<Value, E> {
        let mut stack = Vec::new();
        loop {
            let token = self
                .next(work)
                .await?
                .ok_or_else(|| E::from(serde_json::Error::custom("EOF while parsing a value")))?;
            let value = match token {
                JsonToken::StartArray => {
                    stack.push(Frame::Array(Vec::new()));
                    continue;
                }
                JsonToken::StartObject => {
                    stack.push(Frame::Object(serde_json::Map::new(), None));
                    continue;
                }
                JsonToken::Key(encoded) => {
                    let key = string(encoded, work).await?;
                    let Some(Frame::Object(_, pending)) = stack.last_mut() else {
                        unreachable!("the grammar emits keys only inside objects")
                    };
                    *pending = Some(key);
                    continue;
                }
                JsonToken::EndArray => {
                    let Some(Frame::Array(values)) = stack.pop() else {
                        unreachable!("the grammar closes its matching array")
                    };
                    Value::Array(values)
                }
                JsonToken::EndObject => {
                    let Some(Frame::Object(values, None)) = stack.pop() else {
                        unreachable!("the grammar closes objects after their values")
                    };
                    Value::Object(values)
                }
                JsonToken::Null => Value::Null,
                JsonToken::Bool(value) => Value::Bool(value),
                JsonToken::String(encoded) => Value::String(string(encoded, work).await?),
                // The dependency's public number constructor preserves arbitrary precision.
                // Its scalar conversion remains a synchronous dependency boundary.
                JsonToken::Number(encoded) => {
                    Value::Number(serde_json::from_slice(encoded).map_err(E::from)?)
                }
            };
            match stack.last_mut() {
                Some(Frame::Array(values)) => values.push(value),
                Some(Frame::Object(values, key)) => {
                    values.insert(key.take().expect("the grammar supplies a key"), value);
                }
                None => return Ok(value),
            }
        }
    }
}

async fn string<E: InputError>(encoded: &[u8], work: &mut impl Work<E>) -> Result<String, E> {
    match decode_json_string_with_work(
        encoded,
        &ProductionControl::uncontrolled(),
        &mut Scheduler(work, std::marker::PhantomData),
    )
    .await
    {
        Ok(value) => Ok(value
            .into_uncontrolled()
            .expect("Analysis owns model allocations")),
        Err(error) => Err(diagnostic(error, encoded, (0, JsonExpected::Value), work).await?),
    }
}

async fn diagnostic<E: InputError>(
    error: Error<E>,
    bytes: &[u8],
    (position, expected): (usize, JsonExpected),
    work: &mut impl Work<E>,
) -> Result<E, E> {
    match error {
        Error::Work(error) => return Ok(error),
        Error::Grammar(JsonReadError::Memory(error)) => {
            return Ok(uqa_core::image_io::ImageIoError::from(error).into());
        }
        Error::Grammar(JsonReadError::Cancelled(error)) => {
            return Ok(uqa_core::image_io::ImageIoError::from(error).into());
        }
        Error::Grammar(JsonReadError::InvalidJson | JsonReadError::Malformed { .. }) => {}
    }
    let reason = if position == bytes.len() {
        match expected {
            JsonExpected::ObjectKeyOrEnd
            | JsonExpected::ObjectSeparator
            | JsonExpected::Key
            | JsonExpected::Colon => "EOF while parsing an object",
            JsonExpected::ArrayValueOrEnd | JsonExpected::ArraySeparator => {
                "EOF while parsing a list"
            }
            _ => "EOF while parsing a value",
        }
    } else {
        "invalid JSON text"
    };
    let mut line = 1;
    let mut column = 0;
    for part in bytes[..position.min(bytes.len())].chunks(4096) {
        work.checkpoint().await?;
        for byte in part {
            if *byte == b'\n' {
                line += 1;
                column = 0;
            } else {
                column += 1;
            }
        }
    }
    if position < bytes.len() {
        column += 1;
    }
    Ok(serde_json::Error::custom(format!("{reason} at line {line} column {column}")).into())
}
