//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exact evaluated values stream through fixed-size private-record chunks.

use super::{chunk_key, failure, invalid, key, Records, CHUNK, VALUE};
use crate::{
    mvcc::{PrivateRecordCursor, PrivateRecordSnapshot},
    StorageBackendError, StorageBackendResult,
};
use std::io::{Read, Write};
use uqa_core::{
    memory::Budgeted,
    value_image::{read_value_image, write_value_image, ValueImageError},
    DocId, Value,
};

pub(super) fn read(
    records: &Records<'_>,
    view: &PrivateRecordSnapshot,
    id: DocId,
    bytes: u64,
) -> StorageBackendResult<Budgeted<Value>> {
    records.payload.check()?;
    let cursor = view
        .cursor(&key(VALUE, id), None, &records.control)
        .map_err(failure)?;
    let mut input = Input {
        records,
        cursor,
        id,
        expected: bytes,
        loaded: 0,
        ordinal: 0,
        buffer: [0; CHUNK],
        start: 0,
        end: 0,
        error: None,
    };
    let value = read_value_image(
        &mut input,
        records.payload.memory(),
        records.payload.cancellation(),
    );
    if let Some(error) = input.error.take() {
        return Err(error);
    }
    let value = value.map_err(value_error)?;
    let mut trailing = [0];
    let end = input.read(&mut trailing);
    if let Some(error) = input.error.take() {
        return Err(error);
    }
    if end.map_err(|error| StorageBackendError::backend("retained value image", error))? != 0
        || input.loaded != bytes
    {
        return Err(invalid("trailing or truncated value image"));
    }
    records.payload.check()?;
    Ok(value)
}

struct Input<'a, 'b> {
    records: &'a Records<'a>,
    cursor: PrivateRecordCursor<'b>,
    id: DocId,
    expected: u64,
    loaded: u64,
    ordinal: u64,
    buffer: [u8; CHUNK],
    start: usize,
    end: usize,
    error: Option<StorageBackendError>,
}

impl Input<'_, '_> {
    fn next(&mut self) -> StorageBackendResult<bool> {
        loop {
            self.records.payload.check()?;
            let Some(entry) = self.cursor.next(&self.records.control).map_err(failure)? else {
                if self.loaded != self.expected {
                    return Err(invalid("truncated retained value"));
                }
                return Ok(false);
            };
            if !entry.metadata().live {
                continue;
            }
            if entry.key() != chunk_key(self.id, self.ordinal) {
                return Err(invalid("unordered value chunk"));
            }
            let record = entry.read(&self.records.control).map_err(failure)?;
            let chunk = record
                .value()
                .ok_or_else(|| invalid("missing value chunk"))?;
            let remaining = self.expected.saturating_sub(self.loaded);
            if chunk.is_empty() || chunk.len() as u64 != remaining.min(CHUNK as u64) {
                return Err(invalid("invalid value chunk length"));
            }
            self.buffer[..chunk.len()].copy_from_slice(chunk);
            self.start = 0;
            self.end = chunk.len();
            self.loaded += chunk.len() as u64;
            self.ordinal += 1;
            return Ok(true);
        }
    }
}

impl Read for Input<'_, '_> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        if self.start == self.end {
            match self.next() {
                Ok(false) => return Ok(0),
                Ok(true) => {}
                Err(error) => {
                    self.error = Some(error);
                    return Err(std::io::Error::other("retained value image read failed"));
                }
            }
        }
        let count = bytes.len().min(self.end - self.start);
        bytes[..count].copy_from_slice(&self.buffer[self.start..self.start + count]);
        self.start += count;
        Ok(count)
    }
}

pub(super) fn write(records: &Records<'_>, id: DocId, value: &Value) -> StorageBackendResult<u64> {
    records.payload.check()?;
    let mut output = Output {
        records,
        id,
        ordinal: 0,
        buffer: [0; CHUNK],
        used: 0,
        error: None,
    };
    let result = write_value_image(
        value,
        &mut output,
        records.payload.memory(),
        records.payload.cancellation(),
    );
    if let Some(error) = output.error.take() {
        return Err(error);
    }
    let bytes = result.map_err(value_error)?;
    output.publish()?;
    records.payload.check()?;
    Ok(bytes)
}

struct Output<'a> {
    records: &'a Records<'a>,
    id: DocId,
    ordinal: u64,
    buffer: [u8; CHUNK],
    used: usize,
    error: Option<StorageBackendError>,
}

impl Output<'_> {
    fn publish(&mut self) -> StorageBackendResult<()> {
        self.records.payload.check()?;
        if self.used != 0 {
            self.records.write(
                &chunk_key(self.id, self.ordinal),
                Some(&self.buffer[..self.used]),
            )?;
            self.used = 0;
            self.ordinal += 1;
        }
        Ok(())
    }
}

impl Write for Output<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let count = bytes.len().min(CHUNK - self.used);
        self.buffer[self.used..self.used + count].copy_from_slice(&bytes[..count]);
        self.used += count;
        if self.used == CHUNK {
            if let Err(error) = self.publish() {
                self.error = Some(error);
                return Err(std::io::Error::other("retained value image write failed"));
            }
        }
        Ok(count)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn value_error(error: ValueImageError) -> StorageBackendError {
    match error {
        ValueImageError::Memory(error) => error.into(),
        ValueImageError::Cancelled(error) => error.into(),
        error => StorageBackendError::backend("retained value image", error),
    }
}
