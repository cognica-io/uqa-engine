//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Streaming checkpoint I/O charges decoded buffers and checks cancellation between bounded chunks.

use std::io::{Read, Write};

use sha2::{Digest, Sha256};
use uqa_core::memory::BudgetedVec;

use crate::{
    mvcc::{VersionError, VersionResult},
    read_control::StorageReadControl,
    StorageBackendError,
};

pub(super) fn invalid() -> VersionError {
    VersionError::InvalidEncoding("invalid serializable checkpoint")
}

fn io_error(error: std::io::Error) -> VersionError {
    VersionError::Storage(StorageBackendError::backend(
        "serializable checkpoint",
        error,
    ))
}

pub(super) struct Encoder<'a> {
    output: &'a mut dyn Write,
    digest: Sha256,
    control: &'a StorageReadControl,
}

impl<'a> Encoder<'a> {
    pub(super) fn new(output: &'a mut dyn Write, control: &'a StorageReadControl) -> Self {
        Self {
            output,
            digest: Sha256::new(),
            control,
        }
    }

    pub(super) fn bytes(&mut self, bytes: &[u8]) -> VersionResult<()> {
        for chunk in bytes.chunks(8192) {
            self.control.check()?;
            self.output.write_all(chunk).map_err(io_error)?;
            self.digest.update(chunk);
        }
        Ok(())
    }

    pub(super) fn byte(&mut self, value: u8) -> VersionResult<()> {
        self.bytes(&[value])
    }

    pub(super) fn number(&mut self, value: u64) -> VersionResult<()> {
        self.bytes(&value.to_be_bytes())
    }

    pub(super) fn count(&mut self, value: usize) -> VersionResult<()> {
        self.number(u64::try_from(value).map_err(|_| invalid())?)
    }

    pub(super) fn key(&mut self, bytes: &[u8]) -> VersionResult<()> {
        self.count(bytes.len())?;
        self.bytes(bytes)
    }

    pub(super) fn finish(self) -> VersionResult<()> {
        self.control.check()?;
        self.output
            .write_all(&self.digest.finalize())
            .map_err(io_error)
    }
}

pub(super) struct Decoder<'a> {
    input: &'a mut dyn Read,
    digest: Sha256,
    pub(super) control: &'a StorageReadControl,
}

impl<'a> Decoder<'a> {
    pub(super) fn new(input: &'a mut dyn Read, control: &'a StorageReadControl) -> Self {
        Self {
            input,
            digest: Sha256::new(),
            control,
        }
    }

    fn bytes(&mut self, bytes: &mut [u8]) -> VersionResult<()> {
        for chunk in bytes.chunks_mut(8192) {
            self.control.check()?;
            self.input.read_exact(chunk).map_err(io_error)?;
            self.digest.update(chunk);
        }
        Ok(())
    }

    pub(super) fn array<const N: usize>(&mut self) -> VersionResult<[u8; N]> {
        let mut bytes = [0; N];
        self.bytes(&mut bytes)?;
        Ok(bytes)
    }

    pub(super) fn byte(&mut self) -> VersionResult<u8> {
        Ok(self.array::<1>()?[0])
    }

    pub(super) fn number(&mut self) -> VersionResult<u64> {
        Ok(u64::from_be_bytes(self.array()?))
    }

    pub(super) fn count(&mut self) -> VersionResult<usize> {
        usize::try_from(self.number()?).map_err(|_| invalid())
    }

    pub(super) fn key(&mut self) -> VersionResult<BudgetedVec<u8>> {
        let length = self.count()?;
        let mut bytes = BudgetedVec::new(self.control.memory());
        bytes.reserve(length)?;
        let zeros = [0; 8192];
        while bytes.len() < length {
            self.control.check()?;
            bytes.extend_from_slice(&zeros[..(length - bytes.len()).min(zeros.len())])?;
        }
        self.bytes(&mut bytes)?;
        Ok(bytes)
    }

    pub(super) fn finish(self) -> VersionResult<()> {
        self.control.check()?;
        let mut checksum = [0; 32];
        self.input.read_exact(&mut checksum).map_err(io_error)?;
        let expected: [u8; 32] = self.digest.finalize().into();
        if checksum != expected {
            return Err(invalid());
        }
        loop {
            self.control.check()?;
            match self.input.read(&mut [0]) {
                Ok(0) => return Ok(()),
                Ok(_) => return Err(invalid()),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) => return Err(io_error(error)),
            }
        }
    }
}
