//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Typed numbers preserve the public scalar parser's classifications and consumed boundary.

use super::{
    AsyncImageInput, JsonDiagnostic, JsonInputError, JsonReader, JsonSyntaxKind, ReadStep,
};
use serde::{de, Deserializer};
use std::fmt::{self, Write};

const SCALAR_SCRATCH: usize = 256;

enum Number {
    Unsigned(u64),
    Signed(i64),
    Float(f64),
}

struct Observe;

impl de::Visitor<'_> for Observe {
    type Value = Number;

    fn expecting(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.write_str("u32")
    }

    fn visit_u64<E: de::Error>(self, value: u64) -> Result<Number, E> {
        Ok(Number::Unsigned(value))
    }

    fn visit_i64<E: de::Error>(self, value: i64) -> Result<Number, E> {
        Ok(Number::Signed(value))
    }

    fn visit_f64<E: de::Error>(self, value: f64) -> Result<Number, E> {
        Ok(Number::Float(value))
    }
}

struct Syntax {
    kind: JsonSyntaxKind,
    line: usize,
    column: usize,
}

struct Message {
    bytes: [u8; 128],
    length: usize,
}

impl Write for Message {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        let end = self.length.checked_add(text.len()).ok_or(fmt::Error)?;
        self.bytes
            .get_mut(self.length..end)
            .ok_or(fmt::Error)?
            .copy_from_slice(text.as_bytes());
        self.length = end;
        Ok(())
    }
}

fn observe(input: &[u8]) -> Result<(Result<Number, Syntax>, usize), JsonInputError> {
    let mut parser = serde_json::Deserializer::from_slice(input);
    let number = parser.deserialize_u32(Observe);
    let consumed = parser.into_iter::<de::IgnoredAny>().byte_offset();
    let number = match number {
        Ok(number) => Ok(number),
        Err(error) => {
            // This visitor accepts every numeric classification. Only fixed numeric syntax messages reach this bounded public formatter.
            let mut message = Message {
                bytes: [0; 128],
                length: 0,
            };
            write!(message, "{error}").map_err(|_| JsonInputError::Invalid)?;
            let message = &message.bytes[..message.length];
            let kind = if error.is_eof() {
                JsonSyntaxKind::EndOfValue
            } else if message.starts_with(JsonSyntaxKind::InvalidNumber.message().as_bytes()) {
                JsonSyntaxKind::InvalidNumber
            } else if message.starts_with(JsonSyntaxKind::NumberOutOfRange.message().as_bytes()) {
                JsonSyntaxKind::NumberOutOfRange
            } else {
                return Err(JsonInputError::Invalid);
            };
            Err(Syntax {
                kind,
                line: error.line(),
                column: error.column(),
            })
        }
    };
    Ok((number, consumed))
}

impl JsonReader<'_, '_> {
    /// Read an unsigned 32-bit scalar at its original typed boundary. Numeric conversion uses the public scalar parser under a fixed scratch allowance; cursor advancement retains the enclosing scheduler and original source positions.
    pub async fn read_u32_from(
        &mut self,
        work: &mut AsyncImageInput<'_>,
    ) -> Result<u32, JsonInputError> {
        if !matches!(
            self.next_step(work, false)
                .await
                .map_err(|error| JsonInputError::from_reader(self, error))?,
            ReadStep::Value
        ) {
            return Err(JsonInputError::Invalid);
        }
        if !matches!(self.peek(), Some(b'-' | b'0'..=b'9')) {
            return Err(self.type_error_from("u32", work).await?.into());
        }
        let start = self.position();
        let scratch = work.memory.reserve(SCALAR_SCRATCH)?;
        let (number, consumed) = observe(&self.input[self.position..])?;
        drop(scratch);
        for _ in 0..consumed {
            self.advance(work).await?;
        }
        self.check()?;
        match number {
            Ok(Number::Unsigned(value)) => u32::try_from(value)
                .map_err(|_| JsonDiagnostic::invalid_unsigned(value, self.position()).into()),
            Ok(Number::Signed(value)) => u32::try_from(value)
                .map_err(|_| JsonDiagnostic::invalid_signed(value, self.position()).into()),
            Ok(Number::Float(value)) => {
                Err(JsonDiagnostic::invalid_float(value, self.position()).into())
            }
            Err(error) => {
                let line = start.line + error.line - 1;
                let column = if error.line == 1 {
                    start.column + error.column
                } else {
                    error.column
                };
                let position = self.position();
                let peek = position.line != line || position.column != column;
                let error = self.reject(error.kind, peek);
                Err(JsonInputError::from_reader(self, error))
            }
        }
    }
}

#[cfg(test)]
mod tests;
