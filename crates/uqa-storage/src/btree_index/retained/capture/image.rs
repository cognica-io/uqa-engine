//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bounded native chunk validation precedes exact evaluated-value output.

use super::super::records::{chunk_key, invalid, key, CHUNK, VALUE};
use super::{failure, RetainedValueIndexSnapshot};
use crate::{
    image_io::{ImagePreparation, ImageRead, ImageReader},
    mvcc::PrivateRecordCursor,
    read_control::StorageReadControl,
    StorageBackendResult,
};
use uqa_core::DocId;

pub(super) fn reader<'a>(
    source: &'a RetainedValueIndexSnapshot,
    id: DocId,
    expected: u64,
    control: &StorageReadControl,
) -> StorageBackendResult<ImageReader<'a>> {
    let cursor = source
        .view
        .cursor(&key(VALUE, id), None, control)
        .map_err(failure)?;
    let reader = ImageReader::new(
        Reader {
            source,
            control: control.clone(),
            cursor: Some(cursor),
            id,
            expected,
            validated: 0,
            ordinal: 0,
            loaded: 0,
            buffer: [0; CHUNK],
            start: 0,
            end: 0,
        },
        control,
    )?;
    source.check(control)?;
    Ok(reader)
}

struct Reader<'a> {
    source: &'a RetainedValueIndexSnapshot,
    control: StorageReadControl,
    cursor: Option<PrivateRecordCursor<'a>>,
    id: DocId,
    expected: u64,
    validated: u64,
    ordinal: u64,
    loaded: u64,
    buffer: [u8; CHUNK],
    start: usize,
    end: usize,
}

impl ImageRead for Reader<'_> {
    fn prepare(&mut self, maximum_positions: usize) -> StorageBackendResult<ImagePreparation> {
        self.source.check(&self.control)?;
        let Some(cursor) = &mut self.cursor else {
            return Ok(ImagePreparation {
                positions: 0,
                ready: true,
            });
        };
        for position in 1..=maximum_positions {
            self.source.check(&self.control)?;
            let Some(entry) = cursor.next(&self.control).map_err(failure)? else {
                if self.validated != self.expected {
                    return Err(invalid("truncated retained value"));
                }
                self.cursor = None;
                self.source.check(&self.control)?;
                return Ok(ImagePreparation {
                    positions: position,
                    ready: true,
                });
            };
            if entry.key().len() != 17 {
                return Err(invalid("invalid value chunk identity"));
            }
            let Some(length) = entry.value_len() else {
                continue;
            };
            if entry.key() != chunk_key(self.id, self.ordinal) {
                return Err(invalid("unordered value chunk"));
            }
            if length == 0 || length != (self.expected - self.validated).min(CHUNK as u64) {
                return Err(invalid("invalid value chunk length"));
            }
            self.validated += length;
            self.ordinal += 1;
        }
        self.source.check(&self.control)?;
        Ok(ImagePreparation {
            positions: maximum_positions,
            ready: false,
        })
    }

    fn read(&mut self, output: &mut [u8]) -> StorageBackendResult<usize> {
        self.source.check(&self.control)?;
        if output.is_empty() {
            return Ok(0);
        }
        if self.start == self.end {
            if self.loaded == self.expected {
                return Ok(0);
            }
            let entry = self
                .source
                .view
                .entry(
                    &chunk_key(self.id, self.loaded / CHUNK as u64),
                    &self.control,
                )
                .map_err(failure)?
                .ok_or_else(|| invalid("missing value chunk"))?;
            let remaining = (self.expected - self.loaded).min(CHUNK as u64);
            if entry.value_len() != Some(remaining) {
                return Err(invalid("invalid value chunk length"));
            }
            let record = entry.read(&self.control).map_err(failure)?;
            let bytes = record
                .value()
                .ok_or_else(|| invalid("missing value chunk"))?;
            if bytes.len() as u64 != remaining {
                return Err(invalid("invalid value chunk length"));
            }
            self.buffer[..bytes.len()].copy_from_slice(bytes);
            self.start = 0;
            self.end = bytes.len();
            self.loaded += bytes.len() as u64;
        }
        let count = output.len().min(self.end - self.start);
        output[..count].copy_from_slice(&self.buffer[self.start..self.start + count]);
        self.start += count;
        self.source.check(&self.control)?;
        Ok(count)
    }
}
