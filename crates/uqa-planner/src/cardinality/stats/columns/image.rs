//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native statistics images retain the exact distributions consulted by the planner.

use super::{ColumnStatistics, ColumnStats, SQLError};
use std::{io::Write, slice::Iter};
use uqa_core::{
    image_io::ImageOutput,
    memory::{MemoryBudget, MemoryReservation},
    value_image::{ValueImageError, ValueImageReader},
    CancellationToken, Value,
};

mod input;
#[cfg(test)]
mod tests;

const MAGIC: [u8; 8] = *b"UQACST01";

fn invalid(message: &'static str) -> SQLError {
    SQLError::Routine {
        sqlstate: "XX001".into(),
        message: format!("invalid column statistics image: {message}"),
    }
}

fn value_error(error: ValueImageError) -> SQLError {
    match error {
        ValueImageError::Invalid(message) => invalid(message),
        ValueImageError::Memory(error) => error.into(),
        ValueImageError::Cancelled(error) => error.into(),
        ValueImageError::Io(error) => SQLError::Host(std::sync::Arc::new(error)),
    }
}

#[derive(Clone, Copy)]
enum Field<'a> {
    Bytes(&'a [u8]),
    Scalar([u8; 50], usize),
}

impl Field<'_> {
    fn scalar<const N: usize>(value: [u8; N]) -> Self {
        let mut bytes = [0; 50];
        bytes[..N].copy_from_slice(&value);
        Self::Scalar(bytes, N)
    }

    fn bytes(&self) -> &[u8] {
        match self {
            Self::Bytes(bytes) => bytes,
            Self::Scalar(bytes, length) => &bytes[..*length],
        }
    }
}

struct Distribution<'a> {
    min: Option<&'a Value>,
    max: Option<&'a Value>,
    histogram: Iter<'a, Value>,
    common: Iter<'a, Value>,
    frequencies: Iter<'a, f64>,
}

impl<'a> Distribution<'a> {
    fn next_value(&mut self) -> Option<&'a Value> {
        self.min
            .take()
            .or_else(|| self.max.take())
            .or_else(|| self.histogram.next())
            .or_else(|| self.common.next())
    }
}

#[derive(Clone, Copy)]
enum State {
    Active,
    Complete,
    Failed,
}

struct ValueReader<'a> {
    reader: Box<ValueImageReader<'a, 'a>>,
    _memory: MemoryReservation,
}

/// Borrowed native distributions with admitted active value traversal. Reads emit at most 16 KiB and check cancellation between at most 4 KiB copies; no column or value corpus is copied. Numeric values retain Core's controlled formatting contract.
pub struct ColumnStatisticsImageReader<'a, I> {
    entries: I,
    fields: [Field<'a>; 3],
    field: usize,
    offset: usize,
    distribution: Option<Distribution<'a>>,
    value: Option<ValueReader<'a>>,
    memory: &'a MemoryBudget,
    cancellation: &'a CancellationToken,
    state: State,
}

impl ColumnStatistics {
    /// Select this immutable generation without walking its columns. The enclosing capture owns its original retention allowance.
    pub fn image_reader<'a>(
        &'a self,
        memory: &'a MemoryBudget,
        cancellation: &'a CancellationToken,
    ) -> Result<
        ColumnStatisticsImageReader<'a, impl Iterator<Item = (&'a String, &'a ColumnStats)>>,
        SQLError,
    > {
        cancellation.check()?;
        Ok(ColumnStatisticsImageReader {
            entries: self.iter(),
            fields: [
                Field::scalar(MAGIC),
                Field::scalar((self.len() as u64).to_le_bytes()),
                Field::Bytes(&[]),
            ],
            field: 0,
            offset: 0,
            distribution: None,
            value: None,
            memory,
            cancellation,
            state: State::Active,
        })
    }

    /// Reconstruct a privately owned immutable generation. Every payload and map node keeps its original decoding allowance through shared clones; no analysis, sorting or publication is performed.
    pub fn read_image(
        reader: &mut dyn std::io::Read,
        memory: &MemoryBudget,
        cancellation: &CancellationToken,
    ) -> Result<Self, SQLError> {
        input::read(reader, memory, cancellation)
    }
}

impl<'a, I: Iterator<Item = (&'a String, &'a ColumnStats)>> ColumnStatisticsImageReader<'a, I> {
    fn next_fields(&mut self) -> Result<bool, SQLError> {
        self.fields = [Field::Bytes(&[]); 3];
        self.field = 0;
        self.offset = 0;
        if let Some(distribution) = &mut self.distribution {
            if let Some(value) = distribution.next_value() {
                let reservation = self.memory.reserve(size_of::<ValueImageReader<'_, '_>>())?;
                self.value = Some(ValueReader {
                    reader: Box::new(ValueImageReader::new(value, self.memory, self.cancellation)),
                    _memory: reservation,
                });
                return Ok(true);
            }
            if let Some(frequency) = distribution.frequencies.next() {
                self.fields[0] = Field::scalar(frequency.to_bits().to_le_bytes());
                return Ok(true);
            }
            self.distribution = None;
        }
        let Some((name, stats)) = self.entries.next() else {
            return Ok(false);
        };
        let mut header = [0; 50];
        header[..8].copy_from_slice(&stats.distinct_count.to_le_bytes());
        header[8..16].copy_from_slice(&stats.null_count.to_le_bytes());
        header[16..24].copy_from_slice(&stats.row_count.to_le_bytes());
        header[24] = u8::from(stats.min_value.is_some());
        header[25] = u8::from(stats.max_value.is_some());
        header[26..34].copy_from_slice(&(stats.histogram.len() as u64).to_le_bytes());
        header[34..42].copy_from_slice(&(stats.mcv_values.len() as u64).to_le_bytes());
        header[42..50].copy_from_slice(&(stats.mcv_frequencies.len() as u64).to_le_bytes());
        self.fields = [
            Field::scalar((name.len() as u64).to_le_bytes()),
            Field::Bytes(name.as_bytes()),
            Field::scalar(header),
        ];
        self.distribution = Some(Distribution {
            min: stats.min_value.as_ref(),
            max: stats.max_value.as_ref(),
            histogram: stats.histogram.iter(),
            common: stats.mcv_values.iter(),
            frequencies: stats.mcv_frequencies.iter(),
        });
        Ok(true)
    }

    /// Empty buffers do not advance active input. Error and unwind permanently retire the cursor and release active traversal storage.
    pub fn read(&mut self, output: &mut [u8]) -> Result<usize, SQLError> {
        match self.state {
            State::Failed => return Err(invalid("retired reader")),
            State::Complete => return Ok(0),
            State::Active => {}
        }
        let mut advance = Advance {
            reader: self,
            succeeded: false,
        };
        let count = advance.reader.read_inner(output)?;
        advance.succeeded = true;
        Ok(count)
    }

    fn read_inner(&mut self, output: &mut [u8]) -> Result<usize, SQLError> {
        self.cancellation.check()?;
        let limit = output.len().min(16 * 1024);
        let mut written = 0;
        while written < limit {
            self.cancellation.check()?;
            if self.field < self.fields.len() {
                let bytes = self.fields[self.field].bytes();
                let count = (bytes.len() - self.offset).min(limit - written).min(4096);
                output[written..written + count]
                    .copy_from_slice(&bytes[self.offset..self.offset + count]);
                self.offset += count;
                written += count;
                if self.offset == bytes.len() {
                    self.field += 1;
                    self.offset = 0;
                }
            } else if let Some(value) = &mut self.value {
                let end = (written + 4096).min(limit);
                let count = value
                    .reader
                    .read(&mut output[written..end])
                    .map_err(value_error)?;
                written += count;
                if count == 0 {
                    self.value = None;
                }
            } else if !self.next_fields()? {
                self.state = State::Complete;
                break;
            }
        }
        self.cancellation.check()?;
        Ok(written)
    }

    pub fn write_to(&mut self, writer: &mut dyn Write) -> Result<u64, SQLError> {
        let mut output = ImageOutput {
            writer,
            cancellation: self.cancellation,
            written: 0,
        };
        let mut advance = Advance {
            reader: self,
            succeeded: false,
        };
        let mut bytes = [0; 4096];
        loop {
            let count = advance.reader.read(&mut bytes)?;
            if count == 0 {
                advance.succeeded = true;
                return Ok(output.written);
            }
            output.bytes(&bytes[..count])?;
        }
    }
}

struct Advance<'a, 'source, I> {
    reader: &'a mut ColumnStatisticsImageReader<'source, I>,
    succeeded: bool,
}

impl<I> Drop for Advance<'_, '_, I> {
    fn drop(&mut self) {
        if !self.succeeded {
            self.reader.value = None;
            self.reader.distribution = None;
            self.reader.state = State::Failed;
        }
    }
}
