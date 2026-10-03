//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Batch order is retained in authenticated temporary records; incomplete appends never become visible.

use super::{Edit, RecordKey, RecordWriteKind, StorageReadControl, VersionError, VersionResult};
use crate::{temporary_file::BlockTemporaryFile, StorageBackendError};
use std::{
    io::{self, IoSlice, Read, Seek, SeekFrom},
    sync::Arc,
};
use uqa_core::memory::{BudgetedVec, MemoryError, MemoryReservation};

const BLOCK: usize = 1024;
const HEADER: usize = 18;

pub(super) struct Journal {
    file: BlockTemporaryFile<BLOCK>,
    length: u64,
    failed: bool,
    _memory: MemoryReservation,
}

impl Journal {
    #[cfg(test)]
    pub(super) fn file(&self) -> &BlockTemporaryFile<BLOCK> {
        &self.file
    }

    pub(super) fn new(control: &StorageReadControl) -> VersionResult<Self> {
        let memory = control
            .memory()
            .reserve(size_of::<Self>() + 2 * BLOCK + HEADER)?;
        Ok(Self {
            file: BlockTemporaryFile::new().map_err(io_error)?,
            length: 0,
            failed: false,
            _memory: memory,
        })
    }

    pub(super) fn push(
        &mut self,
        key: &[u8],
        value: Option<&[u8]>,
        kind: RecordWriteKind,
        prefix: bool,
        control: &StorageReadControl,
    ) -> VersionResult<()> {
        control.check()?;
        self.check()?;
        let key_length = u64::try_from(key.len()).map_err(|_| MemoryError::SizeOverflow)?;
        let bytes = value.unwrap_or_default();
        let value_length = u64::try_from(bytes.len()).map_err(|_| MemoryError::SizeOverflow)?;
        let length = key_length
            .checked_add(value_length)
            .and_then(|n| n.checked_add(HEADER as u64))
            .ok_or(MemoryError::SizeOverflow)?;
        let end = self
            .length
            .checked_add(length)
            .ok_or(MemoryError::SizeOverflow)?;
        let mut header = [0; HEADER];
        header[0] = if prefix {
            0
        } else if value.is_some() {
            2
        } else {
            1
        };
        header[1] = kind.code();
        header[2..10].copy_from_slice(&key_length.to_le_bytes());
        header[10..].copy_from_slice(&value_length.to_le_bytes());
        self.file
            .seek(SeekFrom::Start(self.length))
            .map_err(io_error)?;
        let result = self
            .file
            .write_all_vectored(&mut [
                IoSlice::new(&header),
                IoSlice::new(key),
                IoSlice::new(bytes),
            ])
            .map_err(io_error)
            .and_then(|()| control.check().map_err(Into::into));
        if let Err(error) = result {
            if self.file.set_len(self.length).is_err() {
                self.failed = true;
            }
            return Err(error);
        }
        self.length = end;
        Ok(())
    }

    pub(super) fn visit(
        &self,
        control: &StorageReadControl,
        mut visit: impl FnMut(&Edit) -> VersionResult<()>,
    ) -> VersionResult<()> {
        self.check()?;
        let mut file = self.file.reopen().map_err(io_error)?;
        let mut position = 0;
        while position < self.length {
            control.check()?;
            let mut header = [0; HEADER];
            file.read_exact(&mut header).map_err(io_error)?;
            let action = header[0];
            let kind = RecordWriteKind::from_code(header[1])?;
            let key_length = u64::from_le_bytes(header[2..10].try_into().expect("key length"));
            let value_length = u64::from_le_bytes(header[10..].try_into().expect("value length"));
            position = position
                .checked_add(HEADER as u64)
                .and_then(|p| p.checked_add(key_length))
                .and_then(|p| p.checked_add(value_length))
                .filter(|p| *p <= self.length)
                .ok_or_else(invalid)?;
            if action > 2 || (action != 2 && value_length != 0) {
                return Err(invalid());
            }
            let key = read_bytes(&mut file, key_length, control)?;
            let key = RecordKey::from_budgeted(key);
            let value = if action == 2 {
                Some(Arc::new(read_bytes(&mut file, value_length, control)?))
            } else {
                None
            };
            visit(&Edit {
                key,
                value,
                kind,
                prefix: action == 0,
            })?;
        }
        control.check()?;
        Ok(())
    }

    fn check(&self) -> VersionResult<()> {
        if self.failed {
            Err(invalid())
        } else {
            Ok(())
        }
    }
}

fn read_bytes(
    file: &mut BlockTemporaryFile<BLOCK>,
    length: u64,
    control: &StorageReadControl,
) -> VersionResult<BudgetedVec<u8>> {
    let length = usize::try_from(length).map_err(|_| MemoryError::SizeOverflow)?;
    let mut bytes = BudgetedVec::new(control.memory());
    bytes.reserve(length)?;
    let mut buffer = [0; BLOCK];
    while bytes.len() < length {
        control.check()?;
        let count = (length - bytes.len()).min(BLOCK);
        file.read_exact(&mut buffer[..count]).map_err(io_error)?;
        bytes.extend_from_slice(&buffer[..count])?;
    }
    Ok(bytes)
}

fn invalid() -> VersionError {
    VersionError::InvalidEncoding("invalid evaluated record journal")
}
fn io_error(error: io::Error) -> VersionError {
    StorageBackendError::backend("evaluated record journal", error).into()
}
