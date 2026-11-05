//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrowed, resumable value encoding with a fixed-depth continuation stack.

use super::{ValueImageError, ValueImageResult, MAGIC, MAX_DEPTH, WINDOW};
use crate::{
    memory::{MemoryBudget, Produced},
    ArrayValue, CancellationToken, RecordDatumField, RecordFieldType, RecordValue, RowValue, Value,
};
use std::{collections::btree_map, iter::Empty};

mod encode;
mod header;
use header::Header;

enum Frame<'a> {
    Value(&'a Value, usize),
    Bytes(&'a [u8]),
    Decimal(Produced<String>),
    Values(&'a [Value], usize),
    Array(&'a ArrayValue, usize, usize),
    Row(&'a RowValue, usize, usize),
    RowTypes(&'a [RecordFieldType]),
    Record(&'a RecordValue, usize, usize),
    Map(btree_map::Iter<'a, String, Value>, usize),
    FieldsStart,
    Fields,
    DatumFields(Option<&'a [RecordDatumField]>),
    DatumFieldsNext(&'a [RecordDatumField]),
    DatumFieldMetadata(&'a RecordDatumField),
}

enum Pending<'a> {
    Empty,
    Header(Header, usize),
    Bytes(&'a [u8]),
    Decimal(Produced<String>, usize),
}

impl Pending<'_> {
    fn read(&mut self, output: &mut [u8]) -> usize {
        let (bytes, offset) = match self {
            Self::Empty => return 0,
            Self::Header(header, offset) => (header.as_bytes(), *offset),
            Self::Bytes(bytes) => (*bytes, 0),
            Self::Decimal(text, offset) => (text.as_bytes(), *offset),
        };
        let count = output.len().min(bytes.len() - offset);
        output[..count].copy_from_slice(&bytes[offset..offset + count]);
        if offset + count == bytes.len() {
            *self = Self::Empty;
        } else {
            match self {
                Self::Header(_, offset) | Self::Decimal(_, offset) => *offset += count,
                Self::Bytes(bytes) => *bytes = &bytes[count..],
                Self::Empty => unreachable!(),
            }
        }
        count
    }
}

#[derive(Clone, Copy)]
enum State {
    Active,
    Complete,
    Failed,
}

/// Incrementally encode a borrowed evaluated value or an owner's ordered fields. Each read returns at most 16 KiB and retains its exact position. Payloads remain borrowed, traversal uses a fixed stack bounded by the image's nesting limit, and numerical formatting scratch remains charged to `memory` until consumed or dropped. Numeric formatting uses the existing controlled formatter; its work is not bounded by the requested output length. An error or unwind permanently fails this reader and releases its scratch. Failed output must be discarded by the enclosing publication owner.
pub struct ValueImageReader<'a, 'control, I = Empty<(&'a str, &'a Value)>> {
    frames: [Option<Frame<'a>>; MAX_DEPTH + 4],
    depth: usize,
    pending: Pending<'a>,
    fields: Option<I>,
    expected: usize,
    count: usize,
    previous: Option<&'a str>,
    memory: &'control MemoryBudget,
    pub(super) cancellation: &'control CancellationToken,
    state: State,
}

impl<'a, 'control> ValueImageReader<'a, 'control> {
    /// Borrow one value without visiting it or allocating an encoded copy.
    pub fn new(
        value: &'a Value,
        memory: &'control MemoryBudget,
        cancellation: &'control CancellationToken,
    ) -> Self {
        Self::start(Frame::Value(value, 0), None, memory, cancellation)
    }
}

impl<'a, 'control, I> ValueImageReader<'a, 'control, I>
where
    I: ExactSizeIterator<Item = (&'a str, &'a Value)>,
{
    /// Borrow ordered fields without constructing a map. The iterator's exact length and strictly increasing keys are checked as the image is read.
    pub fn from_fields(
        fields: I,
        memory: &'control MemoryBudget,
        cancellation: &'control CancellationToken,
    ) -> Self {
        Self::start(Frame::FieldsStart, Some(fields), memory, cancellation)
    }

    fn start(
        frame: Frame<'a>,
        fields: Option<I>,
        memory: &'control MemoryBudget,
        cancellation: &'control CancellationToken,
    ) -> Self {
        let mut frames = std::array::from_fn(|_| None);
        frames[0] = Some(frame);
        Self {
            frames,
            depth: 1,
            pending: Pending::Bytes(MAGIC),
            fields,
            expected: 0,
            count: 0,
            previous: None,
            memory,
            cancellation,
            state: State::Active,
        }
    }

    /// Fill a bounded output window. An empty buffer does not advance an active reader; a nonempty buffer returns zero only after completion. Completed reads remain at EOF even if cancellation arrives later.
    pub fn read(&mut self, output: &mut [u8]) -> ValueImageResult<usize> {
        match self.state {
            State::Complete => return Ok(0),
            State::Failed => return Err(ValueImageError::Invalid("failed image reader")),
            State::Active => {}
        }
        let mut advance = Advance {
            reader: self,
            succeeded: false,
        };
        let count = advance.reader.read_active(output)?;
        advance.succeeded = true;
        Ok(count)
    }

    fn read_active(&mut self, output: &mut [u8]) -> ValueImageResult<usize> {
        self.cancellation.check()?;
        let limit = output.len().min(WINDOW);
        let mut written = 0;
        while written < limit {
            self.cancellation.check()?;
            written += self.pending.read(&mut output[written..limit]);
            if written == limit {
                break;
            }
            if self.depth == 0 {
                self.state = State::Complete;
                self.fields = None;
                break;
            }
            self.depth -= 1;
            let frame = self.frames[self.depth]
                .take()
                .expect("encoder continuation");
            self.advance(frame)?;
        }
        self.cancellation.check()?;
        Ok(written)
    }

    fn push(&mut self, frame: Frame<'a>) -> ValueImageResult<()> {
        let slot = self
            .frames
            .get_mut(self.depth)
            .ok_or(ValueImageError::Invalid("nesting depth"))?;
        *slot = Some(frame);
        self.depth += 1;
        Ok(())
    }

    fn fail(&mut self) {
        self.state = State::Failed;
        self.pending = Pending::Empty;
        for frame in &mut self.frames[..self.depth] {
            *frame = None;
        }
        self.depth = 0;
        self.fields = None;
    }
}

struct Advance<'r, 'a, 'control, I: ExactSizeIterator<Item = (&'a str, &'a Value)>> {
    reader: &'r mut ValueImageReader<'a, 'control, I>,
    succeeded: bool,
}

impl<'a, I: ExactSizeIterator<Item = (&'a str, &'a Value)>> Drop for Advance<'_, 'a, '_, I> {
    fn drop(&mut self) {
        if !self.succeeded {
            self.reader.fail();
        }
    }
}
