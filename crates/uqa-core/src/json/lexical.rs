//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Token scanning borrows input and validates escapes without decoder scratch.

use super::{JsonReadError, JsonReader};

impl<'a> JsonReader<'a, '_> {
    pub(super) fn peek(&self) -> Option<u8> {
        self.input.get(self.position).copied()
    }

    pub(super) fn advance(&mut self) -> Result<(), JsonReadError> {
        self.position += 1;
        if self.position.is_multiple_of(4096) {
            self.check()?;
        }
        Ok(())
    }

    pub(super) fn consume(&mut self, expected: u8) -> Result<(), JsonReadError> {
        if self.peek() != Some(expected) {
            return Err(JsonReadError::InvalidJson);
        }
        self.advance()
    }

    pub(super) fn skip_whitespace(&mut self) -> Result<(), JsonReadError> {
        while self
            .peek()
            .is_some_and(|byte| matches!(byte, b' ' | b'\n' | b'\r' | b'\t'))
        {
            self.advance()?;
        }
        Ok(())
    }

    pub(super) fn keyword(&mut self, word: &[u8]) -> Result<(), JsonReadError> {
        for &byte in word {
            self.consume(byte)?;
        }
        Ok(())
    }

    pub(super) fn string(&mut self) -> Result<&'a [u8], JsonReadError> {
        let start = self.position;
        self.consume(b'"')?;
        while let Some(byte) = self.peek() {
            self.advance()?;
            match byte {
                b'"' => {
                    let encoded = &self.input[start..self.position];
                    if !self.ignored_string_escapes {
                        std::str::from_utf8(encoded).map_err(|_| JsonReadError::InvalidJson)?;
                    }
                    return Ok(encoded);
                }
                b'\\' => match self.peek().ok_or(JsonReadError::InvalidJson)? {
                    b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't' => self.advance()?,
                    b'u' => {
                        self.advance()?;
                        let unit = self.hex_quad()?;
                        if self.ignored_string_escapes {
                            continue;
                        }
                        match unit {
                            0xD800..=0xDBFF => {
                                self.consume(b'\\')?;
                                self.consume(b'u')?;
                                if !(0xDC00..=0xDFFF).contains(&self.hex_quad()?) {
                                    return Err(JsonReadError::InvalidJson);
                                }
                            }
                            0xDC00..=0xDFFF => return Err(JsonReadError::InvalidJson),
                            _ => {}
                        }
                    }
                    _ => return Err(JsonReadError::InvalidJson),
                },
                0..=31 => return Err(JsonReadError::InvalidJson),
                _ => {}
            }
        }
        Err(JsonReadError::InvalidJson)
    }

    fn hex_quad(&mut self) -> Result<u16, JsonReadError> {
        let mut result = 0;
        for _ in 0..4 {
            let digit = match self.peek().ok_or(JsonReadError::InvalidJson)? {
                byte @ b'0'..=b'9' => byte - b'0',
                byte @ b'a'..=b'f' => byte - b'a' + 10,
                byte @ b'A'..=b'F' => byte - b'A' + 10,
                _ => return Err(JsonReadError::InvalidJson),
            };
            result = (result << 4) | u16::from(digit);
            self.advance()?;
        }
        Ok(result)
    }

    pub(super) fn number(&mut self) -> Result<&'a str, JsonReadError> {
        let start = self.position;
        if self.peek() == Some(b'-') {
            self.advance()?;
        }
        match self.peek().ok_or(JsonReadError::InvalidJson)? {
            b'0' => {
                self.advance()?;
                if self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
                    return Err(JsonReadError::InvalidJson);
                }
            }
            b'1'..=b'9' => self.digits()?,
            _ => return Err(JsonReadError::InvalidJson),
        }
        if self.peek() == Some(b'.') {
            self.advance()?;
            self.digits()?;
        }
        if self.peek().is_some_and(|byte| matches!(byte, b'e' | b'E')) {
            self.advance()?;
            if self.peek().is_some_and(|byte| matches!(byte, b'+' | b'-')) {
                self.advance()?;
            }
            self.digits()?;
        }
        std::str::from_utf8(&self.input[start..self.position])
            .map_err(|_| JsonReadError::InvalidJson)
    }

    fn digits(&mut self) -> Result<(), JsonReadError> {
        let start = self.position;
        while self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
            self.advance()?;
        }
        if self.position == start {
            return Err(JsonReadError::InvalidJson);
        }
        Ok(())
    }
}
