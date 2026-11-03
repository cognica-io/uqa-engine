//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bounded image I/O observes cancellation between partial and interrupted attempts.

use super::{invalid, StorageBackendError, StorageBackendResult, StorageReadControl};
use std::io::{Read, Write};
use uqa_core::memory::{Budgeted, BudgetedVec, MemoryError};

const CHUNK: usize = 4096;

fn io_error(error: std::io::Error) -> StorageBackendError {
    StorageBackendError::backend("evaluated text image I/O", error)
}

pub(super) struct Output<'a> {
    pub writer: &'a mut dyn Write,
    pub control: &'a StorageReadControl,
    pub written: u64,
}

impl Output<'_> {
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

pub(super) struct Input<'a> {
    pub reader: &'a mut dyn Read,
    pub control: &'a StorageReadControl,
}

impl Input<'_> {
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
        let length = self.count()?;
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
