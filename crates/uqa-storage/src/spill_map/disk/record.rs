//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bounded authenticated windows borrow an immutable record extent without decoding it.

use super::{invalid, Mutex, Pages, StorageBackendResult, StorageReadControl};
use uqa_core::memory::MemoryError;

pub(crate) struct RecordBytes<'a> {
    pub(super) file: &'a Mutex<Pages>,
    pub(super) offset: u64,
    pub(super) length: usize,
}

impl RecordBytes<'_> {
    pub(crate) fn len(&self) -> usize {
        self.length
    }

    /// Read at most 4 KiB from the retained record. The source owns its existing authenticated cache; no decoded value or complete encoded buffer is allocated.
    pub(crate) fn read_at(
        &self,
        offset: usize,
        output: &mut [u8],
        control: &StorageReadControl,
    ) -> StorageBackendResult<usize> {
        control.check()?;
        let remaining = self
            .length
            .checked_sub(offset)
            .ok_or_else(|| invalid("record read offset"))?;
        let count = output.len().min(remaining).min(4096);
        if count != 0 {
            let offset = self
                .offset
                .checked_add(offset as u64)
                .ok_or(MemoryError::SizeOverflow)?;
            let mut file = self.file.lock();
            let Pages { file, blocks, .. } = &mut *file;
            blocks.read(file, offset, &mut output[..count])?;
        }
        control.check()?;
        Ok(count)
    }

    pub(crate) fn array<const N: usize>(
        &self,
        offset: usize,
        control: &StorageReadControl,
    ) -> StorageBackendResult<[u8; N]> {
        let mut bytes = [0; N];
        if self.read_at(offset, &mut bytes, control)? != N {
            return Err(invalid("truncated record field"));
        }
        Ok(bytes)
    }
}
