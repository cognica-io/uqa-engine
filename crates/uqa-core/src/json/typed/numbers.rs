//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Typed numbers preserve the public scalar parser's classifications and consumed boundary.

use super::{
    AsyncImageInput, JsonDiagnostic, JsonInputError, JsonReadError, JsonReadWork, JsonReader,
    JsonSyntaxKind, ProductionControl, ReadStep,
};
use serde::{de, Deserializer};
use std::fmt::{self, Write};

const SCALAR_SCRATCH: usize = 256;

enum Number {
    Unsigned(u64),
    Signed(i64),
    Float(f64),
}

struct Observe(&'static str);

impl de::Visitor<'_> for Observe {
    type Value = Number;

    fn expecting(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.write_str(self.0)
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

#[derive(Clone, Copy)]
enum Width {
    Byte,
    Word,
}

impl Width {
    fn expected(self) -> &'static str {
        match self {
            Self::Byte => "u8",
            Self::Word => "u32",
        }
    }

    fn maximum(self) -> u64 {
        match self {
            Self::Byte => u8::MAX.into(),
            Self::Word => u32::MAX.into(),
        }
    }
}

fn observe(input: &[u8], width: Width) -> Result<(Result<Number, Syntax>, usize), JsonReadError> {
    let mut parser = serde_json::Deserializer::from_slice(input);
    let visitor = Observe(width.expected());
    let number = match width {
        Width::Byte => parser.deserialize_u8(visitor),
        Width::Word => parser.deserialize_u32(visitor),
    };
    let consumed = parser.into_iter::<de::IgnoredAny>().byte_offset();
    let number = match number {
        Ok(number) => Ok(number),
        Err(error) => {
            // This visitor accepts every numeric classification. Only fixed numeric syntax messages reach this bounded public formatter.
            let mut message = Message {
                bytes: [0; 128],
                length: 0,
            };
            write!(message, "{error}").map_err(|_| JsonReadError::InvalidJson)?;
            let message = &message.bytes[..message.length];
            let kind = if error.is_eof() {
                JsonSyntaxKind::EndOfValue
            } else if message.starts_with(JsonSyntaxKind::InvalidNumber.message().as_bytes()) {
                JsonSyntaxKind::InvalidNumber
            } else if message.starts_with(JsonSyntaxKind::NumberOutOfRange.message().as_bytes()) {
                JsonSyntaxKind::NumberOutOfRange
            } else {
                return Err(JsonReadError::InvalidJson);
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
        let control = ProductionControl::new(work.memory, work.cancellation, work.cancellation);
        self.read_u32_with_work(&control, &mut super::super::work::InputWork(work))
            .await
            .map_err(|error| error.retain_syntax(self))
    }

    /// Read the original unsigned 32-bit scalar through the caller's work and error channels.
    pub async fn read_u32_with_work<W: JsonReadWork>(
        &mut self,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<u32, W::Error>
    where
        W::Error: From<JsonDiagnostic>,
    {
        let value = self
            .read_unsigned_with_work(Width::Word, control, work)
            .await?;
        Ok(u32::try_from(value).expect("validated u32"))
    }

    /// Read the original unsigned byte scalar, retaining its own public scalar parser and range diagnostics.
    pub async fn read_u8_with_work<W: JsonReadWork>(
        &mut self,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<u8, W::Error>
    where
        W::Error: From<JsonDiagnostic>,
    {
        let value = self
            .read_unsigned_with_work(Width::Byte, control, work)
            .await?;
        Ok(u8::try_from(value).expect("validated u8"))
    }

    async fn read_unsigned_with_work<W: JsonReadWork>(
        &mut self,
        width: Width,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<u64, W::Error>
    where
        W::Error: From<JsonDiagnostic>,
    {
        if !matches!(self.next_step(work, false).await?, ReadStep::Value) {
            return Err(JsonReadError::InvalidJson.into());
        }
        let expected = width.expected();
        if !matches!(self.peek(), Some(b'-' | b'0'..=b'9')) {
            return Err(self
                .type_error_with_work(expected, control, work)
                .await?
                .into());
        }
        let start = self.position();
        let scratch = control
            .reserve(SCALAR_SCRATCH)
            .map_err(JsonReadError::from)?;
        let (number, consumed) = observe(&self.input[self.position..], width)?;
        drop(scratch);
        for _ in 0..consumed {
            self.advance(work).await?;
        }
        self.check()?;
        match number {
            Ok(Number::Unsigned(value)) if value <= width.maximum() => Ok(value),
            Ok(Number::Unsigned(value)) => {
                Err(JsonDiagnostic::invalid_unsigned(value, expected, self.position()).into())
            }
            Ok(Number::Signed(value)) => {
                Err(JsonDiagnostic::invalid_signed(value, expected, self.position()).into())
            }
            Ok(Number::Float(value)) => {
                Err(JsonDiagnostic::invalid_float(value, expected, self.position()).into())
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
                Err(error.into())
            }
        }
    }
}

#[cfg(test)]
mod tests;
