//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrowed JSON events share one grammar without choosing a numeric or object representation.

use crate::{
    memory::{Budgeted, BudgetedSmallVec, MemoryBudget, MemoryError, ProductionControl},
    CancellationToken, QueryCancelled,
};

mod lexical;
mod string;
mod work;
use work::Work;
pub use work::Work as JsonReadWork;

pub use string::{
    decode_json_string, decode_json_string_from, decode_json_string_with_control,
    decode_json_string_with_work,
};

#[derive(Debug, thiserror::Error)]
pub enum JsonReadError {
    #[error("invalid JSON text")]
    InvalidJson,
    #[error(transparent)]
    Memory(#[from] MemoryError),
    #[error(transparent)]
    Cancelled(#[from] QueryCancelled),
    /// Valid JSON whose recognized tagged value carrier violates its format.
    #[error("malformed {kind} value: {reason}")]
    Malformed { kind: &'static str, reason: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JsonToken<'a, N = &'a str> {
    Null,
    Bool(bool),
    Number(N),
    /// The validated token includes its quotes and escapes.
    String(&'a [u8]),
    Key(&'a [u8]),
    StartArray,
    EndArray,
    StartObject,
    EndObject,
}

#[derive(Debug, PartialEq, Eq)]
pub struct JsonEvent<'a, N = &'a str> {
    pub token: JsonToken<'a, N>,
    /// Byte offsets in the original text, including quotes or a container delimiter.
    pub range: std::ops::Range<usize>,
}

/// The structural token expected when a reader rejects malformed input. Consumers choose their own diagnostic wording.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JsonExpected {
    Value,
    End,
    ArrayValueOrEnd,
    ArraySeparator,
    ObjectKeyOrEnd,
    ObjectSeparator,
    Key,
    Colon,
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

/// Open containers. A bounded stack keeps ordinary nesting inline and charges only deeper nesting.
enum Stack {
    Unbounded(Vec<Frame>),
    Bounded(BudgetedSmallVec<[Frame; 16]>),
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
    expected: JsonExpected,
    depth_limit: Option<usize>,
    ignored_string_escapes: bool,
    /// The input came from a `str`, so every token between ASCII delimiters is already valid UTF-8.
    validated_utf8: bool,
}

impl<'a, 'c> JsonReader<'a, 'c> {
    pub fn new(input: &'a str, memory: &MemoryBudget, cancellation: &'c CancellationToken) -> Self {
        Self {
            validated_utf8: true,
            ..Self::from_slice(input.as_bytes(), memory, cancellation)
        }
    }

    pub fn from_slice(
        input: &'a [u8],
        memory: &MemoryBudget,
        cancellation: &'c CancellationToken,
    ) -> Self {
        Self {
            input,
            position: 0,
            stack: Stack::Bounded(BudgetedSmallVec::new(memory)),
            cancellation: Some(cancellation),
            production: None,
            root_started: false,
            expected: JsonExpected::Value,
            depth_limit: None,
            ignored_string_escapes: false,
            validated_utf8: false,
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
            expected: JsonExpected::Value,
            depth_limit: None,
            ignored_string_escapes: false,
            validated_utf8: true,
        }
    }

    /// Read the same borrowed grammar under an ordinary or controlled producer, preserving every active cancellation owner within token scans as well as between events.
    pub fn with_control(input: &'a str, control: &ProductionControl<'c>) -> Self {
        Self {
            validated_utf8: true,
            ..Self::from_slice_with_control(input.as_bytes(), control)
        }
    }

    /// Read borrowed bytes under the original producer; UTF-8 is validated within reached string tokens.
    pub fn from_slice_with_control(input: &'a [u8], control: &ProductionControl<'c>) -> Self {
        Self {
            input,
            position: 0,
            stack: control.budget().map_or_else(
                || Stack::Unbounded(Vec::new()),
                |budget| Stack::Bounded(BudgetedSmallVec::new(budget)),
            ),
            cancellation: None,
            production: Some(*control),
            root_started: false,
            expected: JsonExpected::Value,
            depth_limit: None,
            ignored_string_escapes: false,
            validated_utf8: false,
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

    /// Borrow the failure position and expected structural token after `next_event` returns `InvalidJson`. No additional traversal or allocation is required.
    pub const fn syntax_position(&self) -> (usize, JsonExpected) {
        (self.position, self.expected)
    }

    pub fn next_event(&mut self) -> Result<Option<JsonEvent<'a>>, JsonReadError> {
        work::inline(self.next_event_with_work(&mut ()))?
            .map(JsonEvent::text_number)
            .transpose()
    }

    /// Resume the same borrowed grammar through the original image work channel. Tokens keep their original source ranges; number tokens expose validated ASCII bytes so constructing a borrowed token never rescans an unbounded numeric literal. No source reads are performed.
    pub async fn next_event_from(
        &mut self,
        input: &mut crate::image_io::AsyncImageInput<'_>,
    ) -> Result<Option<JsonEvent<'a, &'a [u8]>>, JsonReadError> {
        self.next_event_with_work(input).await
    }

    /// Resume borrowed token scanning with the enclosing operation's scheduler and error type.
    pub async fn next_event_with_work<W: Work>(
        &mut self,
        work: &mut W,
    ) -> Result<Option<JsonEvent<'a, &'a [u8]>>, W::Error> {
        work.checkpoint().await?;
        self.check()?;
        self.skip_whitespace(work).await?;
        let frame = self.stack.frames().last().copied();
        self.expected = match frame {
            None if self.root_started => JsonExpected::End,
            None | Some(Frame::ArrayValue) => JsonExpected::Value,
            Some(Frame::ArrayFirst) => JsonExpected::ArrayValueOrEnd,
            Some(Frame::ArrayAfter) => JsonExpected::ArraySeparator,
            Some(Frame::ObjectFirst) => JsonExpected::ObjectKeyOrEnd,
            Some(Frame::ObjectKey) => JsonExpected::Key,
            Some(Frame::ObjectValue) => JsonExpected::Colon,
            Some(Frame::ObjectAfter) => JsonExpected::ObjectSeparator,
        };
        match frame {
            None if self.root_started => {
                return if self.position == self.input.len() {
                    Ok(None)
                } else {
                    Err(JsonReadError::InvalidJson.into())
                };
            }
            None => self.root_started = true,
            Some(Frame::ArrayFirst) if self.peek() == Some(b']') => {
                return self.close(JsonToken::EndArray).map_err(Into::into)
            }
            Some(Frame::ObjectFirst) if self.peek() == Some(b'}') => {
                return self.close(JsonToken::EndObject).map_err(Into::into)
            }
            Some(Frame::ArrayAfter) => match self.peek() {
                Some(b']') => return self.close(JsonToken::EndArray).map_err(Into::into),
                Some(b',') => {
                    self.advance(work).await?;
                    self.skip_whitespace(work).await?;
                    self.replace(Frame::ArrayValue);
                    self.expected = JsonExpected::Value;
                }
                _ => return Err(JsonReadError::InvalidJson.into()),
            },
            Some(Frame::ObjectAfter) => match self.peek() {
                Some(b'}') => return self.close(JsonToken::EndObject).map_err(Into::into),
                Some(b',') => {
                    self.advance(work).await?;
                    self.skip_whitespace(work).await?;
                    self.replace(Frame::ObjectKey);
                    self.expected = JsonExpected::Key;
                }
                _ => return Err(JsonReadError::InvalidJson.into()),
            },
            _ => {}
        }
        match self.stack.frames().last().copied() {
            Some(Frame::ObjectFirst | Frame::ObjectKey) => {
                let start = self.position;
                let key = self.string(work).await?;
                self.replace(Frame::ObjectValue);
                return Ok(Some(self.event(start, JsonToken::Key(key))));
            }
            Some(Frame::ObjectValue) => {
                self.consume(b':', work).await?;
                self.skip_whitespace(work).await?;
                self.replace(Frame::ObjectAfter);
                self.expected = JsonExpected::Value;
            }
            Some(Frame::ArrayFirst | Frame::ArrayValue) => self.replace(Frame::ArrayAfter),
            _ => {}
        }
        self.value(work).await.map(Some)
    }

    async fn value<W: Work>(&mut self, work: &mut W) -> Result<JsonEvent<'a, &'a [u8]>, W::Error> {
        let start = self.position;
        let token = match self.peek().ok_or(JsonReadError::InvalidJson)? {
            b'n' => {
                self.keyword(b"null", work).await?;
                JsonToken::Null
            }
            b't' => {
                self.keyword(b"true", work).await?;
                JsonToken::Bool(true)
            }
            b'f' => {
                self.keyword(b"false", work).await?;
                JsonToken::Bool(false)
            }
            b'"' => JsonToken::String(self.string(work).await?),
            b'-' | b'0'..=b'9' => JsonToken::Number(self.number(work).await?),
            b'[' | b'{' => {
                if self
                    .depth_limit
                    .is_some_and(|limit| self.stack.frames().len() >= limit)
                {
                    return Err(JsonReadError::InvalidJson.into());
                }
                let array = self.peek() == Some(b'[');
                self.stack.push(if array {
                    Frame::ArrayFirst
                } else {
                    Frame::ObjectFirst
                })?;
                self.advance(work).await?;
                if array {
                    JsonToken::StartArray
                } else {
                    JsonToken::StartObject
                }
            }
            _ => return Err(JsonReadError::InvalidJson.into()),
        };
        self.check()?;
        Ok(self.event(start, token))
    }

    fn replace(&mut self, frame: Frame) {
        *self.stack.frames().last_mut().expect("open JSON container") = frame;
    }

    fn close(
        &mut self,
        token: JsonToken<'a, &'a [u8]>,
    ) -> Result<Option<JsonEvent<'a, &'a [u8]>>, JsonReadError> {
        let start = self.position;
        self.position += 1;
        self.check()?;
        self.stack.pop();
        Ok(Some(self.event(start, token)))
    }

    fn event(&self, start: usize, token: JsonToken<'a, &'a [u8]>) -> JsonEvent<'a, &'a [u8]> {
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

impl<'a> JsonEvent<'a, &'a [u8]> {
    fn text_number(self) -> Result<JsonEvent<'a>, JsonReadError> {
        let token = match self.token {
            JsonToken::Null => JsonToken::Null,
            JsonToken::Bool(value) => JsonToken::Bool(value),
            JsonToken::Number(value) => JsonToken::Number(
                std::str::from_utf8(value).map_err(|_| JsonReadError::InvalidJson)?,
            ),
            JsonToken::String(value) => JsonToken::String(value),
            JsonToken::Key(value) => JsonToken::Key(value),
            JsonToken::StartArray => JsonToken::StartArray,
            JsonToken::EndArray => JsonToken::EndArray,
            JsonToken::StartObject => JsonToken::StartObject,
            JsonToken::EndObject => JsonToken::EndObject,
        };
        Ok(JsonEvent {
            token,
            range: self.range,
        })
    }
}

impl From<crate::image_io::ImageIoError> for JsonReadError {
    fn from(error: crate::image_io::ImageIoError) -> Self {
        use crate::image_io::ImageIoError;
        match error {
            ImageIoError::Memory(error) => Self::Memory(error),
            ImageIoError::Cancelled(error) => Self::Cancelled(error),
            ImageIoError::Invalid | ImageIoError::Io(_) => Self::InvalidJson,
        }
    }
}

#[cfg(test)]
mod tests;
