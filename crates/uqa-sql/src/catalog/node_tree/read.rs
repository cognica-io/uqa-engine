//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Read node fields without executing an expression or consulting a catalog.

use super::{invalid, Field, Node};
use crate::SQLError;

pub fn parse(text: &str) -> Result<Field, SQLError> {
    let mut input = Reader {
        bytes: text.as_bytes(),
        offset: 0,
    };
    let value = input.value(0)?;
    if input.peek().is_some() {
        return Err(invalid("trailing data in node tree"));
    }
    Ok(value)
}

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl Reader<'_> {
    fn peek(&mut self) -> Option<u8> {
        while self
            .bytes
            .get(self.offset)
            .is_some_and(u8::is_ascii_whitespace)
        {
            self.offset += 1;
        }
        self.bytes.get(self.offset).copied()
    }

    fn value(&mut self, depth: usize) -> Result<Field, SQLError> {
        if depth > 256 {
            return Err(SQLError::Routine {
                sqlstate: "54001".into(),
                message: "node tree nesting is too deep".into(),
            });
        }
        match self.peek() {
            Some(b'{') => self.node(depth + 1).map(Field::Node),
            Some(b'(') => self.list(depth + 1).map(Field::List),
            Some(b')' | b'}' | b'[' | b']') | None => Err(invalid("unexpected end of node field")),
            _ => {
                let (token, escaped) = self.token()?;
                if escaped {
                    return Ok(Field::String(token));
                }
                match token.as_str() {
                    "<>" => Ok(Field::Null),
                    "\"\"" => Ok(Field::String(String::new())),
                    _ if self.peek() == Some(b'[') => self.datum(&token),
                    _ => Ok(Field::Atom(token)),
                }
            }
        }
    }

    fn node(&mut self, depth: usize) -> Result<Node, SQLError> {
        self.offset += 1;
        let (kind, escaped) = self.token()?;
        if escaped
            || kind.is_empty()
            || !kind
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
        {
            return Err(invalid("invalid node tag"));
        }
        let mut fields = Vec::new();
        loop {
            if self.peek() == Some(b'}') {
                self.offset += 1;
                break;
            }
            let (field, escaped) = self.token()?;
            let name = field
                .strip_prefix(':')
                .filter(|name| !name.is_empty())
                .ok_or_else(|| invalid("expected a named node field"))?;
            if escaped || fields.iter().any(|(field, _)| field == name) {
                return Err(invalid("invalid or duplicate node field"));
            }
            fields.push((name.to_owned(), self.value(depth)?));
        }
        Ok(Node { kind, fields })
    }

    fn list(&mut self, depth: usize) -> Result<Vec<Field>, SQLError> {
        self.offset += 1;
        let mut values = Vec::new();
        loop {
            if self.peek() == Some(b')') {
                self.offset += 1;
                return Ok(values);
            }
            values.push(self.value(depth)?);
        }
    }

    fn datum(&mut self, length: &str) -> Result<Field, SQLError> {
        let length = length
            .parse()
            .map_err(|_| invalid("invalid Datum length"))?;
        self.offset += 1;
        let mut bytes = Vec::new();
        loop {
            if self.peek() == Some(b']') {
                self.offset += 1;
                return Ok(Field::Datum { length, bytes });
            }
            let (token, escaped) = self.token()?;
            let byte = token
                .parse::<i16>()
                .ok()
                .filter(|byte| (-128..=255).contains(byte))
                .ok_or_else(|| invalid("invalid Datum byte"))?;
            if escaped {
                return Err(invalid("escaped Datum byte"));
            }
            bytes.push(byte.to_le_bytes()[0]);
        }
    }

    fn token(&mut self) -> Result<(String, bool), SQLError> {
        self.peek();
        let mut bytes = Vec::new();
        let mut escaped = false;
        while let Some(&byte) = self.bytes.get(self.offset) {
            if byte.is_ascii_whitespace() || matches!(byte, b'{' | b'}' | b'(' | b')' | b'[' | b']')
            {
                break;
            }
            self.offset += 1;
            if byte == b'\\' {
                escaped = true;
                let &next = self
                    .bytes
                    .get(self.offset)
                    .ok_or_else(|| invalid("unfinished node token escape"))?;
                bytes.push(next);
                self.offset += 1;
            } else {
                bytes.push(byte);
            }
        }
        if bytes.is_empty() {
            return Err(invalid("missing node token"));
        }
        String::from_utf8(bytes)
            .map(|token| (token, escaped))
            .map_err(|_| invalid("invalid UTF-8 node token"))
    }
}
