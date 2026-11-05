//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bounded image I/O observes cancellation between partial and interrupted attempts.

use crate::{read_control::StorageReadControl, StorageBackendError, StorageBackendResult};
use std::io::{Read, Write};
use uqa_core::memory::{Budgeted, BudgetedVec, MemoryError};

mod reader;
mod retained;
pub use reader::{ImageRead, ImageReader};
pub use retained::with_retained_control;

const CHUNK: usize = 4096;

fn invalid() -> StorageBackendError {
    StorageBackendError::Other("invalid owner image stream".into())
}

fn io_error(error: std::io::Error) -> StorageBackendError {
    StorageBackendError::backend("owner image I/O", error)
}

/// Bounded binary output for an owning image codec. Failed output belongs to the enclosing unpublished artifact.
pub struct ImageOutput<'a> {
    pub writer: &'a mut dyn Write,
    pub control: &'a StorageReadControl,
    pub written: u64,
}

impl ImageOutput<'_> {
    /// Reuse Core's evaluated-value map codec. Its borrowed payload writes use at most 16 KiB windows and preserve all value carriers.
    pub fn fields<'a>(
        &mut self,
        fields: impl ExactSizeIterator<Item = (&'a str, &'a uqa_core::Value)>,
    ) -> StorageBackendResult<()> {
        let written = uqa_core::value_image::write_fields_image(
            fields,
            self.writer,
            self.control.memory(),
            self.control.cancellation(),
        )
        .map_err(value_error)?;
        self.written = self
            .written
            .checked_add(written)
            .ok_or(MemoryError::SizeOverflow)?;
        Ok(())
    }

    pub fn bytes(&mut self, mut bytes: &[u8]) -> StorageBackendResult<()> {
        while !bytes.is_empty() {
            self.control.check()?;
            let length = bytes.len().min(CHUNK);
            match self.writer.write(&bytes[..length]) {
                Ok(0) => return Err(io_error(std::io::ErrorKind::WriteZero.into())),
                Ok(count) if count <= length => {
                    self.written = self
                        .written
                        .checked_add(count as u64)
                        .ok_or(MemoryError::SizeOverflow)?;
                    bytes = &bytes[count..];
                }
                Ok(_) => return Err(invalid()),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) => return Err(io_error(error)),
            }
        }
        self.control.check()
    }

    pub fn count(&mut self, count: usize) -> StorageBackendResult<()> {
        self.bytes(
            &u64::try_from(count)
                .map_err(|_| MemoryError::SizeOverflow)?
                .to_le_bytes(),
        )
    }

    pub fn sized(&mut self, bytes: &[u8]) -> StorageBackendResult<()> {
        self.count(bytes.len())?;
        self.bytes(bytes)
    }
}

/// Controlled binary input; payload buffers retain their decoding allowance.
pub struct ImageInput<'a> {
    pub reader: &'a mut dyn Read,
    pub control: &'a StorageReadControl,
}

impl ImageInput<'_> {
    pub fn value(&mut self) -> StorageBackendResult<Budgeted<uqa_core::Value>> {
        uqa_core::value_image::read_value_image(
            self.reader,
            self.control.memory(),
            self.control.cancellation(),
        )
        .map_err(value_error)
    }

    pub fn text(&mut self) -> StorageBackendResult<Budgeted<String>> {
        let (bytes, memory) = self.bytes()?.into_parts();
        let text = String::from_utf8(bytes).map_err(|_| invalid())?;
        Ok(Budgeted::new(text, memory))
    }

    pub fn exact(&mut self, mut bytes: &mut [u8]) -> StorageBackendResult<()> {
        while !bytes.is_empty() {
            self.control.check()?;
            let length = bytes.len().min(CHUNK);
            match self.reader.read(&mut bytes[..length]) {
                Ok(0) => return Err(io_error(std::io::ErrorKind::UnexpectedEof.into())),
                Ok(count) if count <= length => bytes = &mut bytes[count..],
                Ok(_) => return Err(invalid()),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) => return Err(io_error(error)),
            }
        }
        self.control.check()
    }

    pub fn array<const N: usize>(&mut self) -> StorageBackendResult<[u8; N]> {
        let mut bytes = [0; N];
        self.exact(&mut bytes)?;
        Ok(bytes)
    }

    pub fn count(&mut self) -> StorageBackendResult<usize> {
        usize::try_from(u64::from_le_bytes(self.array()?))
            .map_err(|_| MemoryError::SizeOverflow.into())
    }

    pub fn bytes(&mut self) -> StorageBackendResult<Budgeted<Vec<u8>>> {
        self.bytes_with_limit(usize::MAX)
    }

    /// Reject a declared payload beyond the owning format's ceiling before allocating it.
    pub fn bytes_with_limit(&mut self, maximum: usize) -> StorageBackendResult<Budgeted<Vec<u8>>> {
        let length = self.count()?;
        self.control.check_value_size(length, maximum)?;
        let mut bytes = BudgetedVec::new(self.control.memory());
        bytes.reserve(length)?;
        let mut buffer = [0; CHUNK];
        while bytes.len() < length {
            let count = (length - bytes.len()).min(CHUNK);
            self.exact(&mut buffer[..count])?;
            bytes.extend_from_slice(&buffer[..count])?;
        }
        let (bytes, memory) = bytes.into_parts();
        Ok(Budgeted::new(bytes, memory))
    }

    pub fn end(&mut self) -> StorageBackendResult<()> {
        loop {
            self.control.check()?;
            match self.reader.read(&mut [0]) {
                Ok(0) => return self.control.check(),
                Ok(_) => return Err(invalid()),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) => return Err(io_error(error)),
            }
        }
    }
}

fn value_error(error: uqa_core::value_image::ValueImageError) -> StorageBackendError {
    match error {
        uqa_core::value_image::ValueImageError::Memory(error) => error.into(),
        uqa_core::value_image::ValueImageError::Cancelled(error) => error.into(),
        other => StorageBackendError::backend("owner image value", other),
    }
}
