//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered memory import checks required image membership before complete integrity admission.

use super::{read_memory_image_address, DecodedMemoryImageAddress, MemoryImageHeader};
use crate::{
    backup::{
        BackupError, BackupImageReader, BackupLeaseInput, BackupReadStep, BackupResult, BackupRoot,
        BackupSourceStep, ImageFormat, ImageProvider, UnitKind,
    },
    read_control::StorageReadControl,
};

mod membership;

/// A structurally admitted component. Its codec must consume the complete payload and validate its native representation; complete database closure remains a separate obligation.
#[derive(Clone, Copy, Debug)]
pub struct MemoryImageImportUnit<'a> {
    pub address: &'a DecodedMemoryImageAddress,
    pub decoded_bytes: u64,
}

/// Decode the fixed header and ordered component addresses through the repository's complete verifier. Only two decoded addresses are retained, independent of database population. Neither a component nor the completed input is an Engine activation certificate.
pub struct MemoryImageImportReader<'a> {
    reader: Option<BackupImageReader<'a>>,
    root: BackupRoot,
    control: StorageReadControl,
    header: Option<MemoryImageHeader>,
    previous: Option<DecodedMemoryImageAddress>,
    current: Option<DecodedMemoryImageAddress>,
    verified_bytes: u64,
    complete: bool,
}

impl<'a> MemoryImageImportReader<'a> {
    pub fn new(
        lease: impl Into<BackupLeaseInput<'a>>,
        maximum_workspace_bytes: u64,
        control: &StorageReadControl,
    ) -> BackupResult<Self> {
        let lease = lease.into();
        let root = lease.root();
        if root.cut().format != ImageFormat::new(ImageProvider::Memory, 1)?
            || root.cut().sequence.is_some()
        {
            return Err(BackupError::Unsupported("memory import image format"));
        }
        Ok(Self {
            reader: Some(lease.read_image(maximum_workspace_bytes, control)?),
            root,
            control: control.clone(),
            header: None,
            previous: None,
            current: None,
            verified_bytes: 0,
            complete: false,
        })
    }

    pub const fn root(&self) -> BackupRoot {
        self.root
    }

    pub const fn header(&self) -> Option<MemoryImageHeader> {
        self.header
    }

    pub fn current_unit(&self) -> Option<MemoryImageImportUnit<'_>> {
        Some(MemoryImageImportUnit {
            address: self.current.as_ref()?,
            decoded_bytes: self.reader.as_ref()?.current_unit()?.decoded_bytes(),
        })
    }

    /// Bytes checked by the whole-object verification pass, independently of payload reads.
    pub fn verified_object_bytes(&self) -> u64 {
        self.reader.as_ref().map_or(
            self.verified_bytes,
            BackupImageReader::verified_object_bytes,
        )
    }

    /// Admit one image component or one bounded repository verification step. The header is consumed internally; every returned component must reach verified EOF before the next call. Any failure retires the complete input and its partial decoded names.
    pub fn advance(&mut self, maximum_object_bytes: usize) -> BackupResult<BackupSourceStep> {
        let result = self.advance_inner(maximum_object_bytes);
        if result.is_err() && !self.complete {
            self.release(false);
        }
        result
    }

    fn advance_inner(&mut self, maximum: usize) -> BackupResult<BackupSourceStep> {
        if self.complete {
            return Ok(BackupSourceStep::Complete);
        }
        let reader = self.reader.as_mut().ok_or(BackupError::Finished)?;
        if let Some(current) = self.current.take() {
            self.previous = Some(current);
        }
        match reader.advance(maximum)? {
            BackupReadStep::Pending => return Ok(BackupSourceStep::Pending),
            BackupReadStep::Complete => {
                if self.header.is_none() || reader.verified().is_none() {
                    return Err(BackupError::Invalid("missing memory image header"));
                }
                membership::end(self.previous.as_ref())?;
                self.release(true);
                return Ok(BackupSourceStep::Complete);
            }
            BackupReadStep::Unit => (),
        }
        let entry = reader.current_unit().expect("selected repository unit");
        let address = read_memory_image_address(entry.address(), &self.control)?;
        if self.header.is_none() {
            if !matches!(address, DecodedMemoryImageAddress::Header)
                || entry.kind() != UnitKind::Metadata
                || entry.decoded_bytes() != 33
            {
                return Err(BackupError::Invalid("memory image header unit"));
            }
            let mut bytes = [0; 33];
            reader.read_exact_current(&mut bytes)?;
            reader.require_current_end()?;
            let header = MemoryImageHeader::read_image(&mut &bytes[..], &self.control)?;
            if header.source != self.root.cut().source {
                return Err(BackupError::Invalid("memory image source identity"));
            }
            self.header = Some(header);
            return Ok(BackupSourceStep::Pending);
        }
        membership::next(self.previous.as_ref(), &address, entry.kind())?;
        self.current = Some(address);
        Ok(BackupSourceStep::Unit)
    }

    pub fn read_current(&mut self, output: &mut [u8]) -> BackupResult<usize> {
        let result = if self.current.is_some() {
            self.reader.as_mut().unwrap().read_current(output)
        } else {
            Err(BackupError::Finished)
        };
        if result.is_err() && !self.complete {
            self.release(false);
        }
        result
    }

    fn release(&mut self, complete: bool) {
        self.verified_bytes = self.verified_object_bytes();
        self.reader = None;
        self.current = None;
        self.previous = None;
        if !complete {
            self.header = None;
        }
        self.complete = complete;
    }
}

#[cfg(test)]
mod tests;
