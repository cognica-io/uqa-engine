//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use uqa_core::{memory::BudgetedVec, DocId};

use super::temporary::{io_error, File, TemporaryRun};
use super::{
    invalid, DiskANNBuildVector, DiskANNTemporaryBudget, DiskANNTemporaryError,
    DiskANNVectorVersion,
};
use crate::diskann_index::metric::{checkpoint, exact_reason, norms};
use crate::mvcc::{DatabaseId, StorageTransactionId};
use crate::{read_control::StorageReadControl, StorageBackendResult};

const HEADER_BYTES: usize = 44;

pub(super) struct Records {
    file: TemporaryRun,
    dimensions: u32,
    width: usize,
    count: u64,
    side: bool,
}

impl Records {
    pub(super) fn new(
        dimensions: u32,
        side: bool,
        directory: &Path,
        temporary: &DiskANNTemporaryBudget,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        if dimensions == 0 {
            return Err(invalid("dimensions must be positive"));
        }
        let width = (dimensions as usize)
            .checked_mul(4)
            .and_then(|bytes| bytes.checked_add(HEADER_BYTES))
            .ok_or(DiskANNTemporaryError::SizeOverflow)?;
        Ok(Self {
            file: TemporaryRun::new(directory, temporary, control)?,
            dimensions,
            width,
            count: 0,
            side,
        })
    }

    pub(super) fn len(&self) -> u64 {
        self.count
    }

    pub(super) fn push(
        &mut self,
        doc: DocId,
        ordinal: u32,
        version: DiskANNVectorVersion,
        raw: &[f32],
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        control.check()?;
        if raw.len() != self.dimensions as usize {
            return Err(invalid("captured vector dimensions differ"));
        }
        let count = self
            .count
            .checked_add(1)
            .ok_or(DiskANNTemporaryError::SizeOverflow)?;
        let mut bytes = BudgetedVec::new(control.memory());
        bytes.reserve(self.width)?;
        bytes.extend_from_slice(&doc.to_le_bytes())?;
        bytes.extend_from_slice(&ordinal.to_le_bytes())?;
        bytes.extend_from_slice(&version.writer().database().as_bytes())?;
        bytes.extend_from_slice(&version.writer().allocation().to_le_bytes())?;
        bytes.extend_from_slice(&version.revision().to_le_bytes())?;
        for (index, value) in raw.iter().enumerate() {
            checkpoint(index, control)?;
            bytes.extend_from_slice(&value.to_bits().to_le_bytes())?;
        }
        self.file.append(&bytes, control)?;
        self.count = count;
        Ok(())
    }

    pub(super) fn read(
        &self,
        index: u64,
        control: &StorageReadControl,
    ) -> StorageBackendResult<DiskANNBuildVector> {
        control.check()?;
        if index >= self.count {
            return Err(invalid("captured vector address is out of range"));
        }
        let offset = index
            .checked_mul(self.width as u64)
            .ok_or(DiskANNTemporaryError::SizeOverflow)?;
        self.file.read(control, |file| {
            file.seek(SeekFrom::Start(offset)).map_err(io_error)?;
            let mut bytes = self.buffer(control)?;
            read_record(file, &mut bytes, control)?;
            self.decode(&bytes, control)
        })
    }

    pub(super) fn visit(
        &self,
        control: &StorageReadControl,
        visitor: &mut dyn FnMut(DiskANNBuildVector) -> StorageBackendResult<()>,
    ) -> StorageBackendResult<()> {
        self.file.read(control, |file| {
            if self.count == 0 {
                return Ok(());
            }
            let mut bytes = self.buffer(control)?;
            for _ in 0..self.count {
                read_record(file, &mut bytes, control)?;
                visitor(self.decode(&bytes, control)?)?;
            }
            Ok(())
        })
    }

    fn buffer(&self, control: &StorageReadControl) -> StorageBackendResult<BudgetedVec<u8>> {
        let mut bytes = BudgetedVec::new(control.memory());
        bytes.reserve(self.width)?;
        while bytes.len() < self.width {
            control.check()?;
            let count = (self.width - bytes.len()).min(4096);
            bytes.extend_from_slice(&[0; 4096][..count])?;
        }
        Ok(bytes)
    }

    fn decode(
        &self,
        bytes: &[u8],
        control: &StorageReadControl,
    ) -> StorageBackendResult<DiskANNBuildVector> {
        if bytes.len() != self.width {
            return Err(invalid("captured record length differs"));
        }
        let writer = StorageTransactionId::new(
            DatabaseId::from_bytes(field(bytes, 12)?),
            u64::from_le_bytes(field(bytes, 28)?),
        )
        .map_err(|_| invalid("captured origin allocation is invalid"))?;
        let version = DiskANNVectorVersion::new(writer, u64::from_le_bytes(field(bytes, 36)?))?;
        let mut raw = BudgetedVec::new(control.memory());
        raw.reserve(self.dimensions as usize)?;
        for (index, bits) in bytes[HEADER_BYTES..].chunks_exact(4).enumerate() {
            checkpoint(index, control)?;
            raw.push(f32::from_bits(u32::from_le_bytes(
                bits.try_into().expect("four-byte raw scalar"),
            )))?;
        }
        let (norm, _) = norms(self.dimensions, &raw, control)?;
        let exact = exact_reason(norm);
        if exact.is_some() != self.side {
            return Err(invalid("captured vector classification differs"));
        }
        control.check()?;
        Ok(DiskANNBuildVector {
            doc: u64::from_le_bytes(field(bytes, 0)?),
            ordinal: u32::from_le_bytes(field(bytes, 8)?),
            version,
            raw,
            exact,
        })
    }

    #[cfg(test)]
    pub(super) fn path(&self) -> &Path {
        self.file.path()
    }
}

fn read_record(
    file: &mut File,
    bytes: &mut [u8],
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    for part in bytes.chunks_mut(4096) {
        control.check()?;
        file.read_exact(part).map_err(io_error)?;
    }
    control.check()
}

fn field<const N: usize>(bytes: &[u8], offset: usize) -> StorageBackendResult<[u8; N]> {
    bytes
        .get(offset..offset + N)
        .and_then(|field| field.try_into().ok())
        .ok_or_else(|| invalid("incomplete captured field"))
}
