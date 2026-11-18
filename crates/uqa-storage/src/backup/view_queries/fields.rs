//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Framing over physical chunks retains only matching member bytes. SQL validates the selected query; the complete record still belongs to its ordinary restore codec.

use super::{invalid, BackupError, BackupResult, StorageReadControl};
use uqa_core::json::{decode_json_string, JsonReadError};

const KEY_CAPACITY: usize = 2 + b"constraints_json".len() * 6;

#[derive(Clone, Copy)]
pub(super) enum FieldStep {
    Ignore,
    Selected,
    Last,
    Absent,
}

#[derive(Clone, Copy)]
enum Field {
    Definition,
    Query,
    Table(super::BackupTableField),
}

#[derive(Clone, Copy)]
enum Phase {
    Open,
    Member,
    Key,
    Colon,
    Value,
    Inside,
    After,
}

#[derive(Clone, Copy)]
enum LegacyQuery {
    Unknown,
    Ctes,
    Root,
    Complete,
}

impl LegacyQuery {
    fn observe(self, key: &[u8]) -> Self {
        match (self, key) {
            (Self::Complete, _) | (Self::Ctes, b"root") | (Self::Root, b"ctes") => Self::Complete,
            (_, b"ctes") => Self::Ctes,
            (_, b"root") => Self::Root,
            _ => self,
        }
    }
}

pub(super) struct FieldProjector {
    field: Field,
    phase: Phase,
    object: bool,
    index: usize,
    selected: bool,
    key: [u8; KEY_CAPACITY],
    key_len: usize,
    key_escape: bool,
    span: Span,
    legacy: LegacyQuery,
}

impl FieldProjector {
    pub(super) fn definition() -> Self {
        Self::new(Field::Definition)
    }
    pub(super) fn query() -> Self {
        Self::new(Field::Query)
    }

    pub(super) fn table(field: super::BackupTableField) -> Self {
        Self::new(Field::Table(field))
    }

    pub(super) fn allows_missing(&self) -> bool {
        matches!(self.field, Field::Table(_))
    }

    fn new(field: Field) -> Self {
        Self {
            field,
            phase: Phase::Open,
            object: true,
            index: 0,
            selected: false,
            key: [0; KEY_CAPACITY],
            key_len: 0,
            key_escape: false,
            span: Span::default(),
            legacy: LegacyQuery::Unknown,
        }
    }

    pub(super) fn is_legacy(&self) -> bool {
        matches!(self.legacy, LegacyQuery::Complete)
    }

    pub(super) fn feed(
        &mut self,
        byte: u8,
        control: &StorageReadControl,
    ) -> BackupResult<FieldStep> {
        match self.phase {
            Phase::Open => {
                if whitespace(byte) {
                    return Ok(FieldStep::Ignore);
                }
                self.object = match byte {
                    b'{' => true,
                    b'[' => false,
                    _ => return Err(invalid()),
                };
                self.phase = Phase::Member;
            }
            Phase::Member => {
                if whitespace(byte) {
                    return Ok(FieldStep::Ignore);
                }
                if self.close(byte) {
                    return Ok(FieldStep::Absent);
                }
                if !self.object {
                    self.selected = self.index
                        == match self.field {
                            Field::Definition => 0,
                            Field::Query => 1,
                            Field::Table(_) => return Err(invalid()),
                        };
                    if self.index == 0
                        && matches!(self.field, Field::Query)
                        && matches!(byte, b't' | b'f')
                    {
                        self.legacy = LegacyQuery::Complete;
                        return Ok(FieldStep::Absent);
                    }
                    return self.start_value(byte);
                }
                if byte != b'"' {
                    return Err(invalid());
                }
                self.key[0] = byte;
                self.key_len = 1;
                self.key_escape = false;
                self.phase = Phase::Key;
            }
            Phase::Key => {
                if let Some(slot) = self.key.get_mut(self.key_len) {
                    *slot = byte;
                }
                self.key_len = self.key_len.saturating_add(1);
                if self.key_escape {
                    self.key_escape = false;
                } else if byte == b'\\' {
                    self.key_escape = true;
                } else if byte == b'"' {
                    self.end_key(control)?;
                    self.phase = Phase::Colon;
                }
            }
            Phase::Colon => {
                if whitespace(byte) {
                    return Ok(FieldStep::Ignore);
                }
                if byte != b':' {
                    return Err(invalid());
                }
                self.phase = Phase::Value;
            }
            Phase::Value => {
                if !whitespace(byte) {
                    return self.start_value(byte);
                }
            }
            Phase::Inside => {
                if self.span.primitive && (whitespace(byte) || matches!(byte, b',' | b']' | b'}')) {
                    return self.after(byte);
                }
                if self.span.feed(byte)? {
                    self.phase = Phase::After;
                    if self.selected {
                        return Ok(FieldStep::Last);
                    }
                } else if self.selected {
                    return Ok(FieldStep::Selected);
                }
            }
            Phase::After => return self.after(byte),
        }
        Ok(FieldStep::Ignore)
    }

    fn end_key(&mut self, control: &StorageReadControl) -> BackupResult<()> {
        self.selected = false;
        let Some(raw) = self.key.get(..self.key_len) else {
            return Ok(());
        };
        let decoded;
        let key = if raw.contains(&b'\\') {
            decoded = decode_json_string(raw, control.memory(), control.cancellation())
                .map_err(json_error)?;
            decoded.as_bytes()
        } else {
            &raw[1..raw.len() - 1]
        };
        self.legacy = self.legacy.observe(key);
        self.selected = match self.field {
            Field::Definition => key == b"definition_json",
            Field::Query => key == b"query",
            Field::Table(field) => key == field.key(),
        };
        Ok(())
    }

    fn start_value(&mut self, byte: u8) -> BackupResult<FieldStep> {
        if self.selected
            && !match self.field {
                Field::Definition | Field::Table(_) => byte == b'"',
                Field::Query => matches!(byte, b'{' | b'['),
            }
        {
            return Err(invalid());
        }
        self.span = Span::new(byte)?;
        self.phase = Phase::Inside;
        Ok(if self.selected {
            FieldStep::Selected
        } else {
            FieldStep::Ignore
        })
    }

    fn close(&self, byte: u8) -> bool {
        byte == if self.object { b'}' } else { b']' }
    }

    fn after(&mut self, byte: u8) -> BackupResult<FieldStep> {
        self.phase = Phase::After;
        if self.close(byte) {
            return Ok(FieldStep::Absent);
        }
        if byte == b',' {
            self.index = self.index.saturating_add(1);
            self.phase = Phase::Member;
        } else if !whitespace(byte) {
            return Err(invalid());
        }
        Ok(FieldStep::Ignore)
    }
}

const fn whitespace(b: u8) -> bool {
    matches!(b, b' ' | b'\n' | b'\r' | b'\t')
}

#[derive(Default)]
struct Span {
    depth: usize,
    string: bool,
    escaped: bool,
    primitive: bool,
}

impl Span {
    fn new(first: u8) -> BackupResult<Self> {
        let mut span = Self::default();
        match first {
            b'{' | b'[' => span.depth = 1,
            b'"' => span.string = true,
            b'-' | b'0'..=b'9' | b't' | b'f' | b'n' => span.primitive = true,
            _ => return Err(invalid()),
        }
        Ok(span)
    }

    fn feed(&mut self, byte: u8) -> BackupResult<bool> {
        if self.primitive {
            return Ok(false);
        }
        if self.string {
            if self.escaped {
                self.escaped = false;
            } else if byte == b'\\' {
                self.escaped = true;
            } else if byte == b'"' {
                self.string = false;
            }
        } else if matches!(byte, b'{' | b'[') {
            self.depth = self.depth.checked_add(1).ok_or_else(invalid)?;
        } else if matches!(byte, b'}' | b']') {
            self.depth = self.depth.checked_sub(1).ok_or_else(invalid)?;
        } else if byte == b'"' {
            self.string = true;
        }
        Ok(!self.string && self.depth == 0)
    }
}

fn json_error(error: JsonReadError) -> BackupError {
    match error {
        JsonReadError::Memory(error) => error.into(),
        JsonReadError::Cancelled(error) => error.into(),
        _ => invalid(),
    }
}

#[derive(Default)]
pub(super) struct StringBytes {
    started: bool,
    escape: [u8; 14],
    length: usize,
}

#[derive(Default)]
pub(super) struct Bytes {
    bytes: [u8; 4],
    length: usize,
}

impl Bytes {
    pub(super) fn as_slice(&self) -> &[u8] {
        &self.bytes[..self.length]
    }

    fn one(byte: u8) -> Self {
        Self {
            bytes: [byte, 0, 0, 0],
            length: 1,
        }
    }
}

impl StringBytes {
    /// Decode one byte of the outer JSON string. The shared JSON string codec handles Unicode including surrogate pairs. A physical window can end in the middle of any escape.
    pub(super) fn feed(&mut self, byte: u8, control: &StorageReadControl) -> BackupResult<Bytes> {
        if !self.started {
            if byte != b'"' {
                return Err(invalid());
            }
            self.started = true;
            return Ok(Bytes::default());
        }
        if self.length == 0 {
            if byte != b'\\' {
                return Ok(Bytes::one(byte));
            }
            self.escape[..2].copy_from_slice(b"\"\\");
            self.length = 2;
            return Ok(Bytes::default());
        }
        self.escape[self.length] = byte;
        self.length += 1;
        match self.length {
            3 if matches!(byte, b'"' | b'\\' | b'/') => {
                self.length = 0;
                return Ok(Bytes::one(byte));
            }
            3 if byte != b'u' => (),
            7 => {
                // A high UTF-16 surrogate needs the following low surrogate in the same JSON token.
                if self.escape[3].eq_ignore_ascii_case(&b'd')
                    && matches!(self.escape[4], b'8' | b'9' | b'a' | b'A' | b'b' | b'B')
                {
                    return Ok(Bytes::default());
                }
            }
            13 => (),
            _ => return Ok(Bytes::default()),
        }
        self.escape[self.length] = b'"';
        let text = decode_json_string(
            &self.escape[..=self.length],
            control.memory(),
            control.cancellation(),
        )
        .map_err(json_error)?;
        self.length = 0;
        let mut result = Bytes {
            length: text.len(),
            ..Bytes::default()
        };
        if result.length > result.bytes.len() {
            return Err(invalid());
        }
        result.bytes[..text.len()].copy_from_slice(text.as_bytes());
        Ok(result)
    }

    pub(super) fn finish(&self) -> BackupResult<()> {
        if self.started && self.length == 0 {
            Ok(())
        } else {
            Err(invalid())
        }
    }
}
