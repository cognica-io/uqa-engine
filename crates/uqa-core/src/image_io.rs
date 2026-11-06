//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Controlled binary framing shared by owning image codecs; no storage or SQL interpretation.

use crate::{
    memory::{Budgeted, BudgetedVec, MemoryBudget, MemoryError},
    CancellationToken,
};
use std::io::{Read, Write};

const CHUNK: usize = 4096;

#[derive(Debug, thiserror::Error)]
pub enum ImageIoError {
    #[error("invalid owner image stream")]
    Invalid,
    #[error(transparent)]
    Memory(#[from] MemoryError),
    #[error(transparent)]
    Cancelled(#[from] crate::QueryCancelled),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type ImageIoResult<T> = Result<T, ImageIoError>;

fn invalid() -> ImageIoError {
    ImageIoError::Invalid
}
fn io_error(error: std::io::Error) -> ImageIoError {
    error.into()
}

/// Bounded binary output for an owning image codec. Failed output belongs to the enclosing unpublished artifact.
pub struct ImageOutput<'a> {
    pub writer: &'a mut dyn Write,
    pub cancellation: &'a CancellationToken,
    pub written: u64,
}

impl ImageOutput<'_> {
    pub fn bytes(&mut self, bytes: &[u8]) -> ImageIoResult<()> {
        write_bytes(self.writer, self.cancellation, &mut self.written, bytes)
    }

    pub fn count(&mut self, count: usize) -> ImageIoResult<()> {
        self.bytes(
            &u64::try_from(count)
                .map_err(|_| MemoryError::SizeOverflow)?
                .to_le_bytes(),
        )
    }

    pub fn sized(&mut self, bytes: &[u8]) -> ImageIoResult<()> {
        self.count(bytes.len())?;
        self.bytes(bytes)
    }
}

/// Write bounded fragments against the owner's actual counter. Successful partial attempts remain recorded even if a later writer callback unwinds.
pub fn write_bytes(
    writer: &mut dyn Write,
    cancellation: &CancellationToken,
    written: &mut u64,
    mut bytes: &[u8],
) -> ImageIoResult<()> {
    while !bytes.is_empty() {
        cancellation.check()?;
        let length = bytes.len().min(CHUNK);
        match writer.write(&bytes[..length]) {
            Ok(0) => return Err(io_error(std::io::ErrorKind::WriteZero.into())),
            Ok(count) if count <= length => {
                *written = written
                    .checked_add(count as u64)
                    .ok_or(MemoryError::SizeOverflow)?;
                bytes = &bytes[count..];
            }
            Ok(_) => return Err(invalid()),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(io_error(error)),
        }
    }
    cancellation.check().map_err(Into::into)
}

/// Controlled binary input; payload buffers retain their decoding allowance.
pub struct ImageInput<'a> {
    pub memory: &'a MemoryBudget,
    pub reader: &'a mut dyn Read,
    pub cancellation: &'a CancellationToken,
}

impl ImageInput<'_> {
    pub fn text(&mut self) -> ImageIoResult<Budgeted<String>> {
        let (bytes, memory) = self.bytes()?.into_parts();
        let text = String::from_utf8(bytes).map_err(|_| invalid())?;
        Ok(Budgeted::new(text, memory))
    }

    pub fn exact(&mut self, mut bytes: &mut [u8]) -> ImageIoResult<()> {
        while !bytes.is_empty() {
            self.cancellation.check()?;
            let length = bytes.len().min(CHUNK);
            match self.reader.read(&mut bytes[..length]) {
                Ok(0) => return Err(io_error(std::io::ErrorKind::UnexpectedEof.into())),
                Ok(count) if count <= length => bytes = &mut bytes[count..],
                Ok(_) => return Err(invalid()),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) => return Err(io_error(error)),
            }
        }
        self.cancellation.check().map_err(Into::into)
    }

    pub fn array<const N: usize>(&mut self) -> ImageIoResult<[u8; N]> {
        let mut bytes = [0; N];
        self.exact(&mut bytes)?;
        Ok(bytes)
    }

    pub fn count(&mut self) -> ImageIoResult<usize> {
        usize::try_from(u64::from_le_bytes(self.array()?))
            .map_err(|_| MemoryError::SizeOverflow.into())
    }

    pub fn bytes(&mut self) -> ImageIoResult<Budgeted<Vec<u8>>> {
        self.bytes_with_limit(usize::MAX)
    }

    /// Reject a declared payload beyond the owning format's ceiling before allocating it.
    pub fn bytes_with_limit(&mut self, maximum: usize) -> ImageIoResult<Budgeted<Vec<u8>>> {
        let length = self.count()?;
        self.cancellation.check()?;
        if length > maximum {
            return Err(MemoryError::Limit {
                required: length,
                limit: maximum,
            }
            .into());
        }
        let mut bytes = BudgetedVec::new(self.memory);
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

    pub fn end(&mut self) -> ImageIoResult<()> {
        loop {
            self.cancellation.check()?;
            match self.reader.read(&mut [0]) {
                Ok(0) => return self.cancellation.check().map_err(Into::into),
                Ok(_) => return Err(invalid()),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) => return Err(io_error(error)),
            }
        }
    }
}

#[cfg(test)]
mod tests;
