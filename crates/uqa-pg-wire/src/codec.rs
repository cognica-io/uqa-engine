//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::protocol::{FormatCode, PgWireError};

pub(crate) const MESSAGE_HEADER_LEN: usize = 5;
const LEN_FIELD_SIZE: usize = 4;

pub(crate) fn message_total_len(input: &[u8], has_tag: bool, max_len: usize) -> DecodeLen {
    let prefix_len = if has_tag {
        MESSAGE_HEADER_LEN
    } else {
        LEN_FIELD_SIZE
    };
    if input.len() < prefix_len {
        return DecodeLen::Incomplete;
    }

    let len_offset = usize::from(has_tag);
    let length = i32::from_be_bytes([
        input[len_offset],
        input[len_offset + 1],
        input[len_offset + 2],
        input[len_offset + 3],
    ]);
    if length < LEN_FIELD_SIZE as i32 {
        return DecodeLen::Error(PgWireError::InvalidLength {
            length,
            minimum: LEN_FIELD_SIZE as i32,
        });
    }
    let body_and_len = length as usize;
    if body_and_len > max_len {
        return DecodeLen::Error(PgWireError::MessageTooLarge {
            length,
            maximum: max_len,
        });
    }
    let total = body_and_len + usize::from(has_tag);
    if input.len() < total {
        return DecodeLen::Incomplete;
    }
    DecodeLen::Complete(total)
}

pub(crate) enum DecodeLen {
    Complete(usize),
    Incomplete,
    Error(PgWireError),
}

pub(crate) struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub(crate) const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    pub(crate) fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.pos)
    }

    pub(crate) fn ensure_empty(&self, context: &'static str) -> Result<(), PgWireError> {
        if self.remaining() == 0 {
            Ok(())
        } else {
            Err(PgWireError::TrailingBytes {
                context,
                remaining: self.remaining(),
            })
        }
    }

    pub(crate) fn read_byte(&mut self, context: &'static str) -> Result<u8, PgWireError> {
        let Some(byte) = self.bytes.get(self.pos).copied() else {
            return Err(PgWireError::UnexpectedEof { context });
        };
        self.pos += 1;
        Ok(byte)
    }

    pub(crate) fn read_i16(&mut self, context: &'static str) -> Result<i16, PgWireError> {
        let bytes = self.read_exact(2, context)?;
        Ok(i16::from_be_bytes([bytes[0], bytes[1]]))
    }

    pub(crate) fn read_i32(&mut self, context: &'static str) -> Result<i32, PgWireError> {
        let bytes = self.read_exact(4, context)?;
        Ok(i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    pub(crate) fn read_u32(&mut self, context: &'static str) -> Result<u32, PgWireError> {
        let bytes = self.read_exact(4, context)?;
        Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    pub(crate) fn read_len_i32(
        &mut self,
        context: &'static str,
    ) -> Result<Option<usize>, PgWireError> {
        let length = self.read_i32(context)?;
        if length == -1 {
            return Ok(None);
        }
        if length < 0 {
            return Err(PgWireError::NegativeValue { context });
        }
        Ok(Some(length as usize))
    }

    pub(crate) fn read_exact(
        &mut self,
        length: usize,
        context: &'static str,
    ) -> Result<&'a [u8], PgWireError> {
        let end = self
            .pos
            .checked_add(length)
            .ok_or(PgWireError::LengthTooLarge { context, length })?;
        let Some(slice) = self.bytes.get(self.pos..end) else {
            return Err(PgWireError::UnexpectedEof { context });
        };
        self.pos = end;
        Ok(slice)
    }

    pub(crate) fn read_cstring(&mut self, context: &'static str) -> Result<String, PgWireError> {
        let haystack = &self.bytes[self.pos..];
        let Some(nul) = haystack.iter().position(|byte| *byte == 0) else {
            return Err(PgWireError::MissingNul { context });
        };
        let raw = &haystack[..nul];
        let text = std::str::from_utf8(raw)
            .map_err(|_| PgWireError::InvalidUtf8 { context })?
            .to_owned();
        self.pos += nul + 1;
        Ok(text)
    }
}

pub(crate) struct Writer {
    bytes: Vec<u8>,
}

impl Writer {
    pub(crate) fn new() -> Self {
        Self { bytes: Vec::new() }
    }

    pub(crate) fn with_capacity(capacity: usize) -> Self {
        Self {
            bytes: Vec::with_capacity(capacity),
        }
    }

    pub(crate) fn into_inner(self) -> Vec<u8> {
        self.bytes
    }

    pub(crate) fn write_byte(&mut self, value: u8) {
        self.bytes.push(value);
    }

    pub(crate) fn write_i16(&mut self, value: i16) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    pub(crate) fn write_i32(&mut self, value: i32) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    pub(crate) fn write_u32(&mut self, value: u32) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    pub(crate) fn write_bytes(&mut self, value: &[u8]) {
        self.bytes.extend_from_slice(value);
    }

    pub(crate) fn write_cstring(
        &mut self,
        value: &str,
        context: &'static str,
    ) -> Result<(), PgWireError> {
        if value.as_bytes().contains(&0) {
            return Err(PgWireError::EmbeddedNul { context });
        }
        self.write_bytes(value.as_bytes());
        self.write_byte(0);
        Ok(())
    }

    pub(crate) fn write_format(&mut self, value: FormatCode) {
        self.write_i16(value.as_i16());
    }

    pub(crate) fn frame(tag: u8, body: &[u8]) -> Result<Vec<u8>, PgWireError> {
        let framed_body_len =
            body.len()
                .checked_add(LEN_FIELD_SIZE)
                .ok_or(PgWireError::LengthTooLarge {
                    context: "backend message",
                    length: body.len(),
                })?;
        let length = i32_len(framed_body_len, "backend message")?;
        let capacity = framed_body_len
            .checked_add(1)
            .ok_or(PgWireError::LengthTooLarge {
                context: "backend message",
                length: body.len(),
            })?;
        let mut out = Self::with_capacity(capacity);
        out.write_byte(tag);
        out.write_i32(length);
        out.write_bytes(body);
        Ok(out.into_inner())
    }
}

pub(crate) fn i16_len(count: usize, context: &'static str) -> Result<i16, PgWireError> {
    i16::try_from(count).map_err(|_| PgWireError::CountTooLarge { context, count })
}

pub(crate) fn i32_len(length: usize, context: &'static str) -> Result<i32, PgWireError> {
    i32::try_from(length).map_err(|_| PgWireError::LengthTooLarge { context, length })
}
