//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrowed JSON events share one grammar without choosing a numeric or object representation.

use crate::{
    memory::{Budgeted, BudgetedVec, MemoryBudget, MemoryError, ProductionControl},
    CancellationToken, QueryCancelled,
};

mod lexical;
mod string;

pub use string::{decode_json_string, decode_json_string_with_control};

#[derive(Debug, thiserror::Error)]
pub enum JsonReadError {
    #[error("invalid JSON text")]
    InvalidJson,
    #[error(transparent)]
    Memory(#[from] MemoryError),
    #[error(transparent)]
    Cancelled(#[from] QueryCancelled),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JsonToken<'a> {
    Null,
    Bool(bool),
    Number(&'a str),
    /// The validated token includes its quotes and escapes.
    String(&'a [u8]),
    Key(&'a [u8]),
    StartArray,
    EndArray,
    StartObject,
    EndObject,
}

#[derive(Debug, PartialEq, Eq)]
pub struct JsonEvent<'a> {
    pub token: JsonToken<'a>,
    /// Byte offsets in the original text, including quotes or a container delimiter.
    pub range: std::ops::Range<usize>,
}

#[derive(Clone, Copy)]
enum Frame {
    ArrayFirst,
    ArrayValue,
    ArrayAfter,
    ObjectFirst,
    ObjectKey,
    ObjectValue,
    ObjectAfter,
}

enum Stack {
    Unbounded(Vec<Frame>),
    Bounded(BudgetedVec<Frame>),
}

impl Stack {
    fn frames(&mut self) -> &mut [Frame] {
        match self {
            Self::Unbounded(values) => values,
            Self::Bounded(values) => values,
        }
    }

    fn push(&mut self, frame: Frame) -> Result<(), JsonReadError> {
        match self {
            Self::Unbounded(values) => values.push(frame),
            Self::Bounded(values) => values.push(frame)?,
        }
        Ok(())
    }

    fn pop(&mut self) {
        match self {
            Self::Unbounded(values) => {
                values.pop();
            }
            Self::Bounded(values) => {
                values.pop();
            }
        }
    }
}

/// Iterative structural decoding charges its nesting stack before allocation. Event payloads borrow the input; consumers own and charge their chosen value representation separately.
pub struct JsonReader<'a, 'c> {
    input: &'a [u8],
    position: usize,
    stack: Stack,
    cancellation: Option<&'c CancellationToken>,
    production: Option<ProductionControl<'c>>,
    root_started: bool,
    depth_limit: Option<usize>,
    ignored_string_escapes: bool,
}

impl<'a, 'c> JsonReader<'a, 'c> {
    pub fn new(input: &'a str, memory: &MemoryBudget, cancellation: &'c CancellationToken) -> Self {
        Self::from_slice(input.as_bytes(), memory, cancellation)
    }

    pub fn from_slice(
        input: &'a [u8],
        memory: &MemoryBudget,
        cancellation: &'c CancellationToken,
    ) -> Self {
        Self {
            input,
            position: 0,
            stack: Stack::Bounded(BudgetedVec::new(memory)),
            cancellation: Some(cancellation),
            production: None,
            root_started: false,
            depth_limit: None,
            ignored_string_escapes: false,
        }
    }

    pub(crate) fn unbounded(input: &'a str) -> Self {
        Self {
            input: input.as_bytes(),
            position: 0,
            stack: Stack::Unbounded(Vec::new()),
            cancellation: None,
            production: None,
            root_started: false,
            depth_limit: None,
            ignored_string_escapes: false,
        }
    }

    /// Read the same borrowed grammar under an ordinary or controlled producer, preserving every active cancellation owner within token scans as well as between events.
    pub fn with_control(input: &'a str, control: &ProductionControl<'c>) -> Self {
        Self {
            input: input.as_bytes(),
            position: 0,
            stack: control.budget().map_or_else(
                || Stack::Unbounded(Vec::new()),
                |budget| Stack::Bounded(BudgetedVec::new(budget)),
            ),
            cancellation: None,
            production: Some(*control),
            root_started: false,
            depth_limit: None,
            ignored_string_escapes: false,
        }
    }

    /// Limit open containers for formats whose previous decoder imposed a nesting limit.
    #[must_use]
    pub fn with_depth_limit(mut self, limit: usize) -> Self {
        self.depth_limit = Some(limit);
        self
    }

    /// Match serde's ignored-value byte validation when locating fields in a durable envelope: require four hexadecimal digits after each Unicode escape, without decoding UTF-8 or requiring surrogate pairs in discarded strings. Consumers must decode retained keys and string values with their ordinary strict decoder.
    #[must_use]
    pub fn with_ignored_string_escapes(mut self) -> Self {
        self.ignored_string_escapes = true;
        self
    }

    pub fn next_event(&mut self) -> Result<Option<JsonEvent<'a>>, JsonReadError> {
        self.check()?;
        self.skip_whitespace()?;
        let frame = self.stack.frames().last().copied();
        match frame {
            None if self.root_started => {
                return if self.position == self.input.len() {
                    Ok(None)
                } else {
                    Err(JsonReadError::InvalidJson)
                };
            }
            None => self.root_started = true,
            Some(Frame::ArrayFirst) if self.peek() == Some(b']') => {
                return self.close(JsonToken::EndArray)
            }
            Some(Frame::ObjectFirst) if self.peek() == Some(b'}') => {
                return self.close(JsonToken::EndObject)
            }
            Some(Frame::ArrayAfter) => match self.peek() {
                Some(b']') => return self.close(JsonToken::EndArray),
                Some(b',') => {
                    self.advance()?;
                    self.skip_whitespace()?;
                    self.replace(Frame::ArrayValue);
                }
                _ => return Err(JsonReadError::InvalidJson),
            },
            Some(Frame::ObjectAfter) => match self.peek() {
                Some(b'}') => return self.close(JsonToken::EndObject),
                Some(b',') => {
                    self.advance()?;
                    self.skip_whitespace()?;
                    self.replace(Frame::ObjectKey);
                }
                _ => return Err(JsonReadError::InvalidJson),
            },
            _ => {}
        }
        match self.stack.frames().last().copied() {
            Some(Frame::ObjectFirst | Frame::ObjectKey) => {
                let start = self.position;
                let key = self.string()?;
                self.replace(Frame::ObjectValue);
                return Ok(Some(self.event(start, JsonToken::Key(key))));
            }
            Some(Frame::ObjectValue) => {
                self.consume(b':')?;
                self.skip_whitespace()?;
                self.replace(Frame::ObjectAfter);
            }
            Some(Frame::ArrayFirst | Frame::ArrayValue) => self.replace(Frame::ArrayAfter),
            _ => {}
        }
        self.value().map(Some)
    }

    fn value(&mut self) -> Result<JsonEvent<'a>, JsonReadError> {
        let start = self.position;
        let token = match self.peek().ok_or(JsonReadError::InvalidJson)? {
            b'n' => {
                self.keyword(b"null")?;
                JsonToken::Null
            }
            b't' => {
                self.keyword(b"true")?;
                JsonToken::Bool(true)
            }
            b'f' => {
                self.keyword(b"false")?;
                JsonToken::Bool(false)
            }
            b'"' => JsonToken::String(self.string()?),
            b'-' | b'0'..=b'9' => JsonToken::Number(self.number()?),
            b'[' | b'{' => {
                if self
                    .depth_limit
                    .is_some_and(|limit| self.stack.frames().len() >= limit)
                {
                    return Err(JsonReadError::InvalidJson);
                }
                let array = self.peek() == Some(b'[');
                self.stack.push(if array {
                    Frame::ArrayFirst
                } else {
                    Frame::ObjectFirst
                })?;
                self.advance()?;
                if array {
                    JsonToken::StartArray
                } else {
                    JsonToken::StartObject
                }
            }
            _ => return Err(JsonReadError::InvalidJson),
        };
        self.check()?;
        Ok(self.event(start, token))
    }

    fn replace(&mut self, frame: Frame) {
        *self.stack.frames().last_mut().expect("open JSON container") = frame;
    }

    fn close(&mut self, token: JsonToken<'a>) -> Result<Option<JsonEvent<'a>>, JsonReadError> {
        let start = self.position;
        self.advance()?;
        self.stack.pop();
        Ok(Some(self.event(start, token)))
    }

    fn event(&self, start: usize, token: JsonToken<'a>) -> JsonEvent<'a> {
        JsonEvent {
            token,
            range: start..self.position,
        }
    }

    fn check(&self) -> Result<(), JsonReadError> {
        if let Some(cancellation) = self.cancellation {
            cancellation.check()?;
        }
        if let Some(production) = self.production {
            production.check_cancellation()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
