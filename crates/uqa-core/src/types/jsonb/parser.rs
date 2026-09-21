//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! One native parser supplies ordinary comparison and resource-controlled comparison keys.

use super::{workspace::Workspace, JsonbField, JsonbKeyError, JsonbValue};

pub(super) struct JsonbParser<'a, 'b, 'c> {
    input: &'a [u8],
    position: usize,
    workspace: &'b mut Workspace<'c>,
}

impl<'a, 'b, 'c> JsonbParser<'a, 'b, 'c> {
    pub(super) fn parse(input: &'a str) -> Option<JsonbValue> {
        let mut workspace = Workspace::unbounded();
        JsonbParser::parse_with(input, &mut workspace).ok()
    }

    pub(super) fn parse_with(
        input: &'a str,
        workspace: &'b mut Workspace<'c>,
    ) -> Result<JsonbValue, JsonbKeyError> {
        workspace.check()?;
        let mut parser = Self {
            input: input.as_bytes(),
            position: 0,
            workspace,
        };
        parser.skip_whitespace()?;
        let value = parser.parse_value()?;
        parser.skip_whitespace()?;
        if parser.position != parser.input.len() {
            return Err(JsonbKeyError::InvalidJson);
        }
        parser.workspace.check()?;
        Ok(value)
    }

    fn parse_value(&mut self) -> Result<JsonbValue, JsonbKeyError> {
        self.workspace.check()?;
        Ok(match self.peek().ok_or(JsonbKeyError::InvalidJson)? {
            b'n' => {
                self.consume_keyword(b"null")?;
                JsonbValue::Null
            }
            b't' => {
                self.consume_keyword(b"true")?;
                JsonbValue::Bool(true)
            }
            b'f' => {
                self.consume_keyword(b"false")?;
                JsonbValue::Bool(false)
            }
            b'"' => JsonbValue::String(self.parse_string()?),
            b'[' => return self.parse_array(),
            b'{' => return self.parse_object(),
            b'-' | b'0'..=b'9' => return self.parse_number(),
            _ => return Err(JsonbKeyError::InvalidJson),
        })
    }

    fn advance(&mut self) -> Result<(), JsonbKeyError> {
        self.position += 1;
        if self.position.is_multiple_of(4096) {
            self.workspace.check()?;
        }
        Ok(())
    }

    fn parse_string(&mut self) -> Result<String, JsonbKeyError> {
        let start = self.position;
        self.consume(b'"')?;
        let mut escaped = false;
        while let Some(byte) = self.peek() {
            self.advance()?;
            if escaped {
                escaped = false;
                continue;
            }
            match byte {
                b'\\' => escaped = true,
                b'"' => return self.workspace.string(&self.input[start..self.position]),
                _ => {}
            }
        }
        Err(JsonbKeyError::InvalidJson)
    }

    fn parse_number(&mut self) -> Result<JsonbValue, JsonbKeyError> {
        let start = self.position;
        if self.peek() == Some(b'-') {
            self.advance()?;
        }
        match self.peek().ok_or(JsonbKeyError::InvalidJson)? {
            b'0' => {
                self.advance()?;
                if self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
                    return Err(JsonbKeyError::InvalidJson);
                }
            }
            b'1'..=b'9' => self.consume_digits()?,
            _ => return Err(JsonbKeyError::InvalidJson),
        }
        if self.peek() == Some(b'.') {
            self.advance()?;
            self.consume_digits()?;
        }
        if self.peek().is_some_and(|byte| matches!(byte, b'e' | b'E')) {
            self.advance()?;
            if self.peek().is_some_and(|byte| matches!(byte, b'+' | b'-')) {
                self.advance()?;
            }
            self.consume_digits()?;
        }
        let text = std::str::from_utf8(&self.input[start..self.position])
            .map_err(|_| JsonbKeyError::InvalidJson)?;
        self.workspace.number(text).map(JsonbValue::Number)
    }

    fn consume_digits(&mut self) -> Result<(), JsonbKeyError> {
        let start = self.position;
        while self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
            self.advance()?;
        }
        if self.position == start {
            return Err(JsonbKeyError::InvalidJson);
        }
        Ok(())
    }

    fn parse_array(&mut self) -> Result<JsonbValue, JsonbKeyError> {
        self.consume(b'[')?;
        self.skip_whitespace()?;
        let mut values = self.workspace.buffer();
        if self.peek() == Some(b']') {
            self.advance()?;
            return Ok(JsonbValue::Array(values.finish(self.workspace)));
        }
        loop {
            values.push(self.parse_value()?)?;
            self.skip_whitespace()?;
            match self.peek().ok_or(JsonbKeyError::InvalidJson)? {
                b',' => {
                    self.advance()?;
                    self.skip_whitespace()?;
                }
                b']' => {
                    self.advance()?;
                    return Ok(JsonbValue::Array(values.finish(self.workspace)));
                }
                _ => return Err(JsonbKeyError::InvalidJson),
            }
        }
    }

    fn parse_object(&mut self) -> Result<JsonbValue, JsonbKeyError> {
        self.consume(b'{')?;
        self.skip_whitespace()?;
        let mut values = self.workspace.buffer();
        if self.peek() == Some(b'}') {
            self.advance()?;
            return Ok(JsonbValue::Object(values.finish(self.workspace)));
        }
        loop {
            let position = self.position;
            let name = self.parse_string()?;
            self.skip_whitespace()?;
            self.consume(b':')?;
            self.skip_whitespace()?;
            let value = self.parse_value()?;
            values.push(JsonbField {
                name,
                value,
                position,
            })?;
            self.skip_whitespace()?;
            match self.peek().ok_or(JsonbKeyError::InvalidJson)? {
                b',' => {
                    self.advance()?;
                    self.skip_whitespace()?;
                }
                b'}' => {
                    self.advance()?;
                    let mut values = values.finish(self.workspace);
                    // Keep the last occurrence of each key, then retain lexical key order for the native equality representation. Sorting needs no temporary allocation.
                    values.sort_unstable_by(|left, right| {
                        left.name
                            .cmp(&right.name)
                            .then_with(|| right.position.cmp(&left.position))
                    });
                    values.dedup_by(|later, earlier| later.name == earlier.name);
                    return Ok(JsonbValue::Object(values));
                }
                _ => return Err(JsonbKeyError::InvalidJson),
            }
        }
    }

    fn consume_keyword(&mut self, keyword: &[u8]) -> Result<(), JsonbKeyError> {
        let end = self
            .position
            .checked_add(keyword.len())
            .ok_or(JsonbKeyError::InvalidJson)?;
        if self.input.get(self.position..end) != Some(keyword) {
            return Err(JsonbKeyError::InvalidJson);
        }
        self.position = end;
        Ok(())
    }

    fn consume(&mut self, expected: u8) -> Result<(), JsonbKeyError> {
        if self.peek() != Some(expected) {
            return Err(JsonbKeyError::InvalidJson);
        }
        self.advance()
    }

    fn skip_whitespace(&mut self) -> Result<(), JsonbKeyError> {
        while self
            .peek()
            .is_some_and(|byte| matches!(byte, b' ' | b'\n' | b'\r' | b'\t'))
        {
            self.advance()?;
        }
        Ok(())
    }

    fn peek(&self) -> Option<u8> {
        self.input.get(self.position).copied()
    }
}
