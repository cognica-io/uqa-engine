//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stream retained JSONB without reparsing, sorting or recursively allocating a JSON tree.

use super::{
    aligned, corrupt, internal, word, Produced, ProductionControl, Result, SQLError, ARRAY,
    COUNT_MASK, FALSE, HAS_OFFSET, NULL, NUMERIC, OBJECT, SCALAR, TRUE, TYPE_MASK,
};
use crate::expr::json::production::{writer, Values};
use uqa_core::memory::ProductionString;

pub(super) fn decode(bytes: &[u8], control: &ProductionControl<'_>) -> Result<Produced<String>> {
    control.check()?;
    let mut output = ProductionString::new(*control);
    let mut frames = Values::new(control);
    frames.push(Frame::new(bytes, &mut output, control)?, control)?;
    while let Some(frame) = frames.as_mut_slice().last_mut() {
        control.check()?;
        if let Some(entry) = frame.next(&mut output, control)? {
            if let Some(child) = entry.write(&mut output, control)? {
                frames.push(Frame::new(child, &mut output, control)?, control)?;
            }
        } else {
            if let Some(close) = frame.close {
                output.push(close)?;
            }
            frames.truncate(frames.len() - 1);
        }
    }
    Ok(output.finish()?)
}

struct Frame<'a> {
    entries: &'a [u8],
    data: &'a [u8],
    count: usize,
    step: usize,
    object: bool,
    close: Option<char>,
    key_offset: usize,
    value_offset: usize,
}

impl<'a> Frame<'a> {
    fn new(
        bytes: &'a [u8],
        output: &mut ProductionString<'_>,
        control: &ProductionControl<'_>,
    ) -> Result<Self> {
        let header = word(bytes, 0)?;
        let object = match header & (ARRAY | OBJECT) {
            ARRAY => false,
            OBJECT => true,
            _ => return Err(internal("unknown type of jsonb container")),
        };
        let count = (header & COUNT_MASK) as usize;
        let slots = count
            .checked_mul(if object { 2 } else { 1 })
            .ok_or_else(corrupt)?;
        let end = slots
            .checked_mul(4)
            .and_then(|length| length.checked_add(4))
            .ok_or_else(corrupt)?;
        let entries = bytes.get(4..end).ok_or_else(corrupt)?;
        let data = bytes.get(end..).ok_or_else(corrupt)?;
        let mut value_offset = 0;
        if object {
            for index in 0..count {
                control.check()?;
                value_offset = entry_end(word(entries, index * 4)?, value_offset)?;
            }
        }
        let close = if object {
            output.push('{')?;
            Some('}')
        } else if header & SCALAR == 0 {
            output.push('[')?;
            Some(']')
        } else {
            None
        };
        Ok(Self {
            entries,
            data,
            count,
            step: 0,
            object,
            close,
            key_offset: 0,
            value_offset,
        })
    }

    fn next(
        &mut self,
        output: &mut ProductionString<'_>,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Entry<'a>>> {
        control.check()?;
        let limit = self.count * if self.object { 2 } else { 1 };
        if self.step == limit {
            return Ok(None);
        }
        let key = self.object && self.step.is_multiple_of(2);
        if self.object && !key {
            output.push_str(": ")?;
        } else if self.step != 0 {
            output.push_str(", ")?;
        }
        let (index, offset) = if key {
            (self.step / 2, &mut self.key_offset)
        } else if self.object {
            (self.count + self.step / 2, &mut self.value_offset)
        } else {
            (self.step, &mut self.value_offset)
        };
        let word = word(self.entries, index * 4)?;
        let end = entry_end(word, *offset)?;
        let entry = Entry {
            data: self.data,
            start: *offset,
            end,
            kind: word & TYPE_MASK,
            key,
        };
        *offset = end;
        self.step += 1;
        Ok(Some(entry))
    }
}

fn entry_end(entry: u32, start: usize) -> Result<usize> {
    let length = (entry & COUNT_MASK) as usize;
    if entry & HAS_OFFSET != 0 {
        Ok(length)
    } else {
        start.checked_add(length).ok_or_else(corrupt)
    }
}

struct Entry<'a> {
    data: &'a [u8],
    start: usize,
    end: usize,
    kind: u32,
    key: bool,
}

impl<'a> Entry<'a> {
    fn write(
        &self,
        output: &mut ProductionString<'_>,
        control: &ProductionControl<'_>,
    ) -> Result<Option<&'a [u8]>> {
        match self.kind {
            NULL => output.push_str("null")?,
            TRUE => output.push_str("true")?,
            FALSE => output.push_str("false")?,
            0 => {
                let bytes = self.data.get(self.start..self.end).ok_or_else(corrupt)?;
                let text = std::str::from_utf8(bytes).map_err(|error| SQLError::Routine {
                    sqlstate: "22021".into(),
                    message: format!(
                        "invalid byte sequence for encoding \"UTF8\": 0x{:02x}",
                        bytes[error.valid_up_to()]
                    ),
                })?;
                writer::write_string(text, output, control)?;
            }
            NUMERIC => {
                // Numeric output follows its own varlena length, not the enclosing JEntry length.
                let bytes = self.data.get(aligned(self.start)?..).ok_or_else(corrupt)?;
                let length = (word(bytes, 0)? >> 2) as usize;
                let payload = bytes.get(4..length).ok_or_else(corrupt)?;
                let text =
                    crate::catalog::node_tree::decode_numeric_datum_with_control(payload, control)?;
                output.push_str(&text)?;
            }
            _ if self.key => return Err(internal("invalid jsonb scalar type")),
            _ => {
                return Ok(Some(
                    self.data.get(aligned(self.start)?..).ok_or_else(corrupt)?,
                ))
            }
        }
        Ok(None)
    }
}
