//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Batch order is retained in authenticated temporary records; incomplete appends never become visible. Records collect in a tail of less than one block, which reaches the file once it fills a block, so a record costs a copy rather than the rewrite of a partial block, and a visit reads each block once.

use super::{Edit, RecordKey, RecordWriteKind, StorageReadControl, VersionError, VersionResult};
use crate::{temporary_file::BlockTemporaryFile, StorageBackendError};
use std::{
    io::{self, BufReader, IoSlice, Read, Seek, SeekFrom},
    sync::Arc,
};
use uqa_core::memory::{BudgetedVec, MemoryError, MemoryReservation};

const BLOCK: usize = 1024;
const HEADER: usize = 18;

pub(super) struct Journal {
    file: BlockTemporaryFile<BLOCK>,
    /// The bytes the file holds, a whole number of blocks.
    written: u64,
    /// The bytes after the file's last block, fewer than a block.
    tail: Vec<u8>,
    failed: bool,
    /// The tail and a visit's block buffer.
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
            written: 0,
            tail: Vec::with_capacity(BLOCK),
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
        let total = u64::try_from(self.tail.len())
            .ok()
            .and_then(|tail| tail.checked_add(length))
            .filter(|total| self.written.checked_add(*total).is_some())
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
        let block = BLOCK as u64;
        if total < block {
            self.tail.extend_from_slice(&header);
            self.tail.extend_from_slice(key);
            self.tail.extend_from_slice(bytes);
            return Ok(());
        }
        let parts: [&[u8]; 4] = [&self.tail, &header, key, bytes];
        // Whole blocks reach the file from the tail and the record; the rest becomes the new tail.
        let full = total - total % block;
        let mut slices = [IoSlice::new(&[]); 4];
        let mut remaining = full;
        for (slice, part) in slices.iter_mut().zip(parts) {
            let taken = remaining.min(part.len() as u64) as usize;
            *slice = IoSlice::new(&part[..taken]);
            remaining -= taken as u64;
        }
        self.file
            .seek(SeekFrom::Start(self.written))
            .map_err(io_error)?;
        let result = self
            .file
            .write_all_vectored(&mut slices)
            .map_err(io_error)
            .and_then(|()| control.check().map_err(Into::into));
        if let Err(error) = result {
            if self.file.set_len(self.written).is_err() {
                self.failed = true;
            }
            return Err(error);
        }
        let mut tail = Vec::with_capacity(BLOCK);
        let mut skipped = full;
        for part in parts {
            let taken = skipped.min(part.len() as u64) as usize;
            skipped -= taken as u64;
            tail.extend_from_slice(&part[taken..]);
        }
        self.written += full;
        self.tail = tail;
        Ok(())
    }

    pub(super) fn visit(
        &self,
        control: &StorageReadControl,
        mut visit: impl FnMut(&Edit) -> VersionResult<()>,
    ) -> VersionResult<()> {
        self.check()?;
        let file = self.file.reopen().map_err(io_error)?.take(self.written);
        let mut reader = BufReader::with_capacity(BLOCK, file).chain(self.tail.as_slice());
        let length = self.written + self.tail.len() as u64;
        let mut position = 0;
        while position < length {
            control.check()?;
            let mut header = [0; HEADER];
            reader.read_exact(&mut header).map_err(io_error)?;
            let action = header[0];
            let kind = RecordWriteKind::from_code(header[1])?;
            let key_length = u64::from_le_bytes(header[2..10].try_into().expect("key length"));
            let value_length = u64::from_le_bytes(header[10..].try_into().expect("value length"));
            position = position
                .checked_add(HEADER as u64)
                .and_then(|p| p.checked_add(key_length))
                .and_then(|p| p.checked_add(value_length))
                .filter(|p| *p <= length)
                .ok_or_else(invalid)?;
            if action > 2 || (action != 2 && value_length != 0) {
                return Err(invalid());
            }
            let key = read_bytes(&mut reader, key_length, control)?;
            let key = RecordKey::from_budgeted(key);
            let value = if action == 2 {
                Some(Arc::new(read_bytes(&mut reader, value_length, control)?))
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
    file: &mut impl Read,
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
