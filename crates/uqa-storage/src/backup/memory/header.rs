//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A fixed memory header retains the source identity and joint catalog allocation cut.

use crate::{
    image_io::{ImageInput, ImageRead, ImageReader},
    mvcc::DatabaseId,
    read_control::StorageReadControl,
    StorageBackendError, StorageBackendResult,
};
use std::io::Read;

const MAGIC: &[u8; 8] = b"UQAMEM01";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemoryImageHeader {
    pub source: DatabaseId,
    pub catalog_oid_position: Option<u64>,
}

impl MemoryImageHeader {
    pub fn image_reader(
        self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<ImageReader<'static>> {
        control.check()?;
        if self.source.as_bytes() == [0; 16] {
            return Err(invalid());
        }
        let mut bytes = [0; 33];
        bytes[..8].copy_from_slice(MAGIC);
        bytes[8..24].copy_from_slice(&self.source.as_bytes());
        bytes[24] = u8::from(self.catalog_oid_position.is_some());
        bytes[25..].copy_from_slice(&self.catalog_oid_position.unwrap_or(0).to_le_bytes());
        ImageReader::new(Output { bytes, offset: 0 }, control)
    }

    pub fn read_image(
        reader: &mut dyn Read,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        let mut input = ImageInput { reader, control };
        let bytes = input.array::<33>()?;
        if &bytes[..8] != MAGIC || bytes[24] > 1 || bytes[8..24] == [0; 16] {
            return Err(invalid());
        }
        let position = u64::from_le_bytes(bytes[25..].try_into().unwrap());
        if bytes[24] == 0 && position != 0 {
            return Err(invalid());
        }
        input.end()?;
        Ok(Self {
            source: DatabaseId::from_bytes(bytes[8..24].try_into().unwrap()),
            catalog_oid_position: (bytes[24] == 1).then_some(position),
        })
    }
}

fn invalid() -> StorageBackendError {
    StorageBackendError::Other("invalid memory image header".into())
}

struct Output {
    bytes: [u8; 33],
    offset: usize,
}

impl ImageRead for Output {
    fn read(&mut self, output: &mut [u8]) -> StorageBackendResult<usize> {
        let count = output.len().min(self.bytes.len() - self.offset);
        output[..count].copy_from_slice(&self.bytes[self.offset..self.offset + count]);
        self.offset += count;
        Ok(count)
    }
}

#[cfg(test)]
mod tests;
