//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Token scanning borrows input and validates escapes without decoder scratch.

use super::{work::Work, JsonReadError, JsonReader};

impl<'a> JsonReader<'a, '_> {
    pub(super) fn peek(&self) -> Option<u8> {
        self.input.get(self.position).copied()
    }

    pub(super) async fn advance<W: Work>(&mut self, work: &mut W) -> Result<(), W::Error> {
        self.position += 1;
        if self.position.is_multiple_of(4096) {
            self.check()?;
            work.checkpoint().await?;
        }
        Ok(())
    }

    pub(super) async fn consume<W: Work>(
        &mut self,
        expected: u8,
        work: &mut W,
    ) -> Result<(), W::Error> {
        if self.peek() != Some(expected) {
            return Err(JsonReadError::InvalidJson.into());
        }
        self.advance(work).await
    }

    pub(super) async fn skip_whitespace<W: Work>(&mut self, work: &mut W) -> Result<(), W::Error> {
        while self
            .peek()
            .is_some_and(|byte| matches!(byte, b' ' | b'\n' | b'\r' | b'\t'))
        {
            self.advance(work).await?;
        }
        Ok(())
    }

    pub(super) async fn keyword<W: Work>(
        &mut self,
        keyword: &[u8],
        work: &mut W,
    ) -> Result<(), W::Error> {
        for &byte in keyword {
            self.consume(byte, work).await?;
        }
        Ok(())
    }

    pub(super) async fn string<W: Work>(&mut self, work: &mut W) -> Result<&'a [u8], W::Error> {
        let start = self.position;
        self.consume(b'"', work).await?;
        while let Some(byte) = self.peek() {
            self.advance(work).await?;
            match byte {
                b'"' => {
                    let encoded = &self.input[start..self.position];
                    if !self.ignored_string_escapes && !self.validated_utf8 {
                        validate_utf8(encoded, work).await?;
                    }
                    return Ok(encoded);
                }
                b'\\' => match self.peek().ok_or(JsonReadError::InvalidJson)? {
                    b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't' => {
                        self.advance(work).await?;
                    }
                    b'u' => {
                        self.advance(work).await?;
                        let unit = self.hex_quad(work).await?;
                        if self.ignored_string_escapes {
                            continue;
                        }
                        match unit {
                            0xD800..=0xDBFF => {
                                self.consume(b'\\', work).await?;
                                self.consume(b'u', work).await?;
                                if !(0xDC00..=0xDFFF).contains(&self.hex_quad(work).await?) {
                                    return Err(JsonReadError::InvalidJson.into());
                                }
                            }
                            0xDC00..=0xDFFF => return Err(JsonReadError::InvalidJson.into()),
                            _ => {}
                        }
                    }
                    _ => return Err(JsonReadError::InvalidJson.into()),
                },
                0..=31 => return Err(JsonReadError::InvalidJson.into()),
                _ => {}
            }
        }
        Err(JsonReadError::InvalidJson.into())
    }

    async fn hex_quad<W: Work>(&mut self, work: &mut W) -> Result<u16, W::Error> {
        let mut result = 0;
        for _ in 0..4 {
            let digit = match self.peek().ok_or(JsonReadError::InvalidJson)? {
                byte @ b'0'..=b'9' => byte - b'0',
                byte @ b'a'..=b'f' => byte - b'a' + 10,
                byte @ b'A'..=b'F' => byte - b'A' + 10,
                _ => return Err(JsonReadError::InvalidJson.into()),
            };
            result = (result << 4) | u16::from(digit);
            self.advance(work).await?;
        }
        Ok(result)
    }

    pub(super) async fn number<W: Work>(&mut self, work: &mut W) -> Result<&'a [u8], W::Error> {
        let start = self.position;
        if self.peek() == Some(b'-') {
            self.advance(work).await?;
        }
        match self.peek().ok_or(JsonReadError::InvalidJson)? {
            b'0' => {
                self.advance(work).await?;
                if self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
                    return Err(JsonReadError::InvalidJson.into());
                }
            }
            b'1'..=b'9' => self.digits(work).await?,
            _ => return Err(JsonReadError::InvalidJson.into()),
        }
        if self.peek() == Some(b'.') {
            self.advance(work).await?;
            self.digits(work).await?;
        }
        if self.peek().is_some_and(|byte| matches!(byte, b'e' | b'E')) {
            self.advance(work).await?;
            if self.peek().is_some_and(|byte| matches!(byte, b'+' | b'-')) {
                self.advance(work).await?;
            }
            self.digits(work).await?;
        }
        Ok(&self.input[start..self.position])
    }

    async fn digits<W: Work>(&mut self, work: &mut W) -> Result<(), W::Error> {
        let start = self.position;
        while self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
            self.advance(work).await?;
        }
        if self.position == start {
            return Err(JsonReadError::InvalidJson.into());
        }
        Ok(())
    }
}

async fn validate_utf8<W: Work>(mut bytes: &[u8], work: &mut W) -> Result<(), W::Error> {
    while !bytes.is_empty() {
        work.checkpoint().await?;
        let length = bytes.len().min(4096);
        let consumed = match std::str::from_utf8(&bytes[..length]) {
            Ok(_) => length,
            Err(error) if error.error_len().is_none() && length < bytes.len() => {
                error.valid_up_to()
            }
            Err(_) => return Err(JsonReadError::InvalidJson.into()),
        };
        bytes = &bytes[consumed..];
    }
    Ok(())
}
