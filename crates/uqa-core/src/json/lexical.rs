//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Token scanning borrows input and validates escapes without decoder scratch.

use super::{work::Work, JsonReader, JsonSyntaxKind};

impl<'a> JsonReader<'a, '_> {
    pub(super) fn peek(&self) -> Option<u8> {
        self.input.get(self.position).copied()
    }

    pub(super) async fn advance<W: Work>(&mut self, work: &mut W) -> Result<(), W::Error> {
        if self.peek() == Some(b'\n') {
            self.line += 1;
            self.line_start = self.position + 1;
        }
        self.position += 1;
        if self.position.is_multiple_of(4096) {
            self.check()?;
            work.checkpoint().await?;
        }
        Ok(())
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
            if self.peek() != Some(byte) {
                let kind = if self.peek().is_none() {
                    JsonSyntaxKind::EndOfValue
                } else {
                    JsonSyntaxKind::ExpectedIdentifier
                };
                return Err(self.reject(kind, true).into());
            }
            self.advance(work).await?;
        }
        Ok(())
    }

    pub(super) async fn string<W: Work>(&mut self, work: &mut W) -> Result<&'a [u8], W::Error> {
        let start = self.position;
        self.advance(work).await?;
        while let Some(byte) = self.peek() {
            self.advance(work).await?;
            match byte {
                b'"' => {
                    let encoded = &self.input[start..self.position];
                    if !self.ignored_string_escapes
                        && !self.validated_utf8
                        && !validate_utf8(encoded, work).await?
                    {
                        return Err(self.reject(JsonSyntaxKind::InvalidUnicode, false).into());
                    }
                    return Ok(encoded);
                }
                b'\\' => match self.peek() {
                    Some(b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't') => {
                        self.advance(work).await?;
                    }
                    Some(b'u') => {
                        self.advance(work).await?;
                        let unit = self.hex_quad(work).await?;
                        if self.ignored_string_escapes {
                            continue;
                        }
                        match unit {
                            0xD800..=0xDBFF => {
                                for expected in [b'\\', b'u'] {
                                    if self.peek() != Some(expected) {
                                        let kind = if self.peek().is_none() {
                                            JsonSyntaxKind::EndOfString
                                        } else {
                                            JsonSyntaxKind::IncompleteSurrogate
                                        };
                                        return Err(self.reject(kind, true).into());
                                    }
                                    self.advance(work).await?;
                                }
                                if !(0xDC00..=0xDFFF).contains(&self.hex_quad(work).await?) {
                                    return Err(self
                                        .reject(JsonSyntaxKind::InvalidSurrogate, false)
                                        .into());
                                }
                            }
                            0xDC00..=0xDFFF => {
                                return Err(self
                                    .reject(JsonSyntaxKind::InvalidSurrogate, false)
                                    .into())
                            }
                            _ => {}
                        }
                    }
                    None => return Err(self.reject(JsonSyntaxKind::EndOfString, false).into()),
                    _ => return Err(self.reject(JsonSyntaxKind::InvalidEscape, true).into()),
                },
                0..=31 => return Err(self.reject(JsonSyntaxKind::ControlCharacter, false).into()),
                _ => {}
            }
        }
        Err(self.reject(JsonSyntaxKind::EndOfString, false).into())
    }

    async fn hex_quad<W: Work>(&mut self, work: &mut W) -> Result<u16, W::Error> {
        if self.input.len() - self.position < 4 {
            while self.peek().is_some() {
                self.advance(work).await?;
            }
            return Err(self.reject(JsonSyntaxKind::EndOfString, false).into());
        }
        let mut result = 0;
        let mut valid = true;
        for _ in 0..4 {
            let digit = match self.peek().expect("complete four-byte escape") {
                byte @ b'0'..=b'9' => byte - b'0',
                byte @ b'a'..=b'f' => byte - b'a' + 10,
                byte @ b'A'..=b'F' => byte - b'A' + 10,
                _ => {
                    valid = false;
                    0
                }
            };
            result = (result << 4) | u16::from(digit);
            self.advance(work).await?;
        }
        if !valid {
            return Err(self.reject(JsonSyntaxKind::InvalidEscape, false).into());
        }
        Ok(result)
    }

    pub(super) async fn number<W: Work>(&mut self, work: &mut W) -> Result<&'a [u8], W::Error> {
        let start = self.position;
        if self.peek() == Some(b'-') {
            self.advance(work).await?;
        }
        match self.peek() {
            Some(b'0') => {
                self.advance(work).await?;
                if self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
                    return Err(self.reject(JsonSyntaxKind::InvalidNumber, true).into());
                }
            }
            Some(b'1'..=b'9') => self.digits(work).await?,
            None => return Err(self.reject(JsonSyntaxKind::EndOfValue, false).into()),
            _ => return Err(self.reject(JsonSyntaxKind::InvalidNumber, true).into()),
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
            let kind = if self.peek().is_none() {
                JsonSyntaxKind::EndOfValue
            } else {
                JsonSyntaxKind::InvalidNumber
            };
            return Err(self.reject(kind, true).into());
        }
        Ok(())
    }
}

async fn validate_utf8<W: Work>(mut bytes: &[u8], work: &mut W) -> Result<bool, W::Error> {
    while !bytes.is_empty() {
        work.checkpoint().await?;
        let length = bytes.len().min(4096);
        let consumed = match std::str::from_utf8(&bytes[..length]) {
            Ok(_) => length,
            Err(error) if error.error_len().is_none() && length < bytes.len() => {
                error.valid_up_to()
            }
            Err(_) => return Ok(false),
        };
        bytes = &bytes[consumed..];
    }
    Ok(true)
}
