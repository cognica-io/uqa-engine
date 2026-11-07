//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Materialized path images preserve evaluated pairs without a graph or path evaluator.

use super::{CapturedPath, CapturedPathIndex, MemoryPaths, PathIndex, PathStorage};
use std::io::{Read, Write};
use uqa_storage::{
    image_io::{ImageOutput, ImageRead, ImageReader},
    read_control::StorageReadControl,
    StorageBackendError, StorageBackendResult,
};

mod input;
mod stream;
#[cfg(test)]
mod tests;

const MAGIC: &[u8; 8] = b"UQAPTH01";

fn invalid() -> StorageBackendError {
    StorageBackendError::Other("invalid materialized path-index image".into())
}

impl PathIndex {
    /// Borrow the exact materialized memory index. Construction and preparation do not visit labels or pairs: the immutable native owner establishes their order and uniqueness when it is built or imported. Persistent indexes and query views belong to their selected storage provider.
    pub fn image_reader<'a>(
        &'a self,
        control: &'a StorageReadControl,
    ) -> StorageBackendResult<ImageReader<'a>> {
        control.check()?;
        reader(&self.memory_paths()?.image, control)
    }

    /// Stream exact label sequences and evaluated pairs through the same bounded codec as suspended reads. No graph, SQL, expression or user callback is consulted.
    pub fn write_image(
        &self,
        output: &mut dyn Write,
        control: &StorageReadControl,
    ) -> StorageBackendResult<u64> {
        control.check()?;
        write(&self.memory_paths()?.image, output, control)
    }

    /// Decode and validate a fresh unpublished memory index. Complete framing, UTF-8 and strictly ordered unique sequences/pairs precede exposure. The native index retains its complete import allowance through clones and captured images.
    pub fn read_image(
        input: &mut dyn Read,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        input::read(input, control)
    }
}

pub(super) fn reader<'a>(
    image: &'a CapturedPathIndex,
    control: &'a StorageReadControl,
) -> StorageBackendResult<ImageReader<'a>> {
    ImageReader::new(stream::PathImageCursor::new(image, control)?, control)
}

pub(super) fn write(
    image: &CapturedPathIndex,
    output: &mut dyn Write,
    control: &StorageReadControl,
) -> StorageBackendResult<u64> {
    let mut cursor = stream::PathImageCursor::new(image, control)?;
    let mut output = ImageOutput {
        writer: output,
        control,
        written: 0,
    };
    let mut buffer = [0; 4096];
    loop {
        let count = cursor.read(&mut buffer)?;
        if count == 0 {
            return Ok(output.written);
        }
        output.bytes(&buffer[..count])?;
    }
}
