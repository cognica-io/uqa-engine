//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Syntax observations retain the original source position without reconstructing or reparsing input.

use super::{JsonReadError, JsonReader};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JsonPosition {
    pub byte_offset: usize,
    pub line: usize,
    pub column: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JsonSyntaxKind {
    EndOfValue,
    EndOfArray,
    EndOfObject,
    EndOfString,
    ExpectedValue,
    ExpectedIdentifier,
    ExpectedColon,
    ExpectedArraySeparator,
    ExpectedObjectSeparator,
    ExpectedKey,
    InvalidNumber,
    NumberOutOfRange,
    InvalidEscape,
    InvalidUnicode,
    InvalidSurrogate,
    IncompleteSurrogate,
    ControlCharacter,
    TrailingComma,
    TrailingCharacters,
    DepthLimit,
}

impl JsonSyntaxKind {
    pub const fn is_eof(self) -> bool {
        matches!(
            self,
            Self::EndOfValue | Self::EndOfArray | Self::EndOfObject | Self::EndOfString
        )
    }

    pub const fn message(self) -> &'static str {
        match self {
            Self::EndOfValue => "EOF while parsing a value",
            Self::EndOfArray => "EOF while parsing a list",
            Self::EndOfObject => "EOF while parsing an object",
            Self::EndOfString => "EOF while parsing a string",
            Self::ExpectedValue => "expected value",
            Self::ExpectedIdentifier => "expected ident",
            Self::ExpectedColon => "expected `:`",
            Self::ExpectedArraySeparator => "expected `,` or `]`",
            Self::ExpectedObjectSeparator => "expected `,` or `}`",
            Self::ExpectedKey => "key must be a string",
            Self::InvalidNumber => "invalid number",
            Self::NumberOutOfRange => "number out of range",
            Self::InvalidEscape => "invalid escape",
            Self::InvalidUnicode => "invalid unicode code point",
            Self::InvalidSurrogate => "lone leading surrogate in hex escape",
            Self::IncompleteSurrogate => "unexpected end of hex escape",
            Self::ControlCharacter => {
                "control character (\\u0000-\\u001F) found while parsing a string"
            }
            Self::TrailingComma => "trailing comma",
            Self::TrailingCharacters => "trailing characters",
            Self::DepthLimit => "recursion limit exceeded",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JsonSyntaxError {
    pub kind: JsonSyntaxKind,
    pub position: JsonPosition,
}

impl std::fmt::Display for JsonSyntaxError {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            output,
            "{} at line {} column {}",
            self.kind.message(),
            self.position.line,
            self.position.column
        )
    }
}

impl std::error::Error for JsonSyntaxError {}

impl JsonReader<'_, '_> {
    /// Source position after the bytes already consumed, with one-based lines and byte columns.
    pub fn position(&self) -> JsonPosition {
        JsonPosition {
            byte_offset: self.position,
            line: self.line,
            column: self.position - self.line_start,
        }
    }

    /// The lexical or structural failure retained by this reader. Cancellation and admission failures do not fabricate a syntax diagnostic.
    pub fn syntax_error(&self) -> Option<JsonSyntaxError> {
        self.syntax_error
    }

    /// Retire a sequence after its typed element failed. The enclosing decoder observes whitespace, a closing delimiter or a comma and following whitespace before assigning a location to the original unlocated data error. It never parses another element or replaces that error with a later syntax failure.
    pub async fn finish_sequence_error_from(
        mut self,
        work: &mut crate::image_io::AsyncImageInput<'_>,
    ) -> Result<JsonPosition, JsonReadError> {
        self.skip_whitespace(work).await?;
        match self.peek() {
            Some(b']') => self.advance(work).await?,
            Some(b',') => {
                self.advance(work).await?;
                self.skip_whitespace(work).await?;
            }
            _ => (),
        }
        Ok(self.position())
    }

    pub(super) fn reject(&mut self, kind: JsonSyntaxKind, peek: bool) -> JsonReadError {
        let mut position = self.position();
        if peek {
            if let Some(byte) = self.peek() {
                position.byte_offset += 1;
                if byte == b'\n' {
                    position.line += 1;
                    position.column = 0;
                } else {
                    position.column += 1;
                }
            }
        }
        self.syntax_error = Some(JsonSyntaxError { kind, position });
        JsonReadError::InvalidJson
    }
}
