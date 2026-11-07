//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native sequence images preserve the captured definition and autonomous position together.

use super::capture::{CapturedSequence, CapturedSequences};
use std::io::Write;
use uqa_core::{image_io::ImageOutput, CancellationToken};
use uqa_sql::SQLError;

mod record;
pub(super) use record::{decode, RECORD_BYTES};

pub(super) const MAGIC: [u8; 8] = *b"UQASEQ01";

pub(super) fn invalid(message: &'static str) -> SQLError {
    SQLError::Routine {
        sqlstate: "XX001".into(),
        message: format!("invalid sequence image: {message}"),
    }
}

#[derive(Clone, Copy)]
enum Field<'a> {
    Bytes(&'a [u8]),
    Scalar([u8; RECORD_BYTES], usize),
}

impl<'a> Field<'a> {
    fn scalar<const N: usize>(bytes: [u8; N]) -> Self {
        let mut value = [0; RECORD_BYTES];
        value[..N].copy_from_slice(&bytes);
        Self::Scalar(value, N)
    }
    fn bytes(&self) -> &[u8] {
        match self {
            Self::Bytes(value) => value,
            Self::Scalar(value, length) => &value[..*length],
        }
    }
    fn text(value: &'a str) -> [Self; 2] {
        [
            Self::scalar((value.len() as u64).to_le_bytes()),
            Self::Bytes(value.as_bytes()),
        ]
    }
}

enum State {
    Active,
    Complete,
    Failed,
}

/// Borrowed sequence metadata output with fixed cursor storage. Each read emits at most 16 KiB and observes cancellation at least every 4 KiB. Its companion authority image is required to reconstruct a catalog.
pub struct SequenceImageReader<'a, I> {
    entries: I,
    fields: [Field<'a>; 5],
    field: usize,
    offset: usize,
    cancellation: &'a CancellationToken,
    state: State,
}

impl CapturedSequences {
    /// Select the immutable cut without walking or copying any sequence. The image includes selected autonomous positions, not session-local cached values.
    pub fn image_reader<'a>(
        &'a self,
        cancellation: &'a CancellationToken,
    ) -> Result<SequenceImageReader<'a, impl Iterator<Item = CapturedSequence<'a>>>, SQLError> {
        cancellation.check()?;
        Ok(SequenceImageReader {
            entries: self.iter(),
            fields: [
                Field::scalar(MAGIC),
                Field::scalar((self.len() as u64).to_le_bytes()),
                Field::Bytes(&[]),
                Field::Bytes(&[]),
                Field::Bytes(&[]),
            ],
            field: 0,
            offset: 0,
            cancellation,
            state: State::Active,
        })
    }
}

impl<'a, I: Iterator<Item = CapturedSequence<'a>>> SequenceImageReader<'a, I> {
    /// Empty output does no work. Any error or unwind retires the cursor permanently.
    pub fn read(&mut self, output: &mut [u8]) -> Result<usize, SQLError> {
        match std::mem::replace(&mut self.state, State::Failed) {
            State::Failed => Err(invalid("retired image reader")),
            State::Complete => {
                self.state = State::Complete;
                Ok(0)
            }
            State::Active => {
                let count = self.read_inner(output)?;
                self.state = if count == 0 && !output.is_empty() {
                    State::Complete
                } else {
                    State::Active
                };
                Ok(count)
            }
        }
    }

    fn read_inner(&mut self, output: &mut [u8]) -> Result<usize, SQLError> {
        self.cancellation.check()?;
        let limit = output.len().min(16 * 1024);
        let mut written = 0;
        while written < limit {
            self.cancellation.check()?;
            if self.field == self.fields.len() {
                let Some(entry) = self.entries.next() else {
                    break;
                };
                let [schema_length, schema] = Field::text(&entry.relation.schema);
                let [name_length, name] = Field::text(&entry.relation.name);
                self.fields = [
                    schema_length,
                    schema,
                    name_length,
                    name,
                    Field::scalar(record::encode(&entry)?),
                ];
                self.field = 0;
                self.offset = 0;
            }
            let bytes = self.fields[self.field].bytes();
            if self.offset == bytes.len() {
                self.field += 1;
                self.offset = 0;
                continue;
            }
            let count = (bytes.len() - self.offset).min(limit - written).min(4096);
            output[written..written + count]
                .copy_from_slice(&bytes[self.offset..self.offset + count]);
            self.offset += count;
            written += count;
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
        let mut bytes = [0; 4096];
        loop {
            let count = self.read(&mut bytes)?;
            if count == 0 {
                return Ok(output.written);
            }
            self.state = State::Failed;
            output.bytes(&bytes[..count])?;
            self.state = State::Active;
        }
    }
}

#[cfg(test)]
mod tests;
