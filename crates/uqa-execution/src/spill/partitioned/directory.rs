//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The open-addressed directory keeps one page resident. Resizing copies only fixed-width entries.

use super::IndexResult;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;
use uqa_storage::temporary_file::BlockTemporaryFile;

const SLOT_BYTES: usize = 32;
const PAGE_BYTES: usize = 256;

#[derive(Clone, Copy, Default)]
pub(super) struct Slot {
    pub(super) hash: u64,
    /// One-based byte position in the key file. Zero denotes an unused slot.
    pub(super) key: u64,
    /// A single row lives in `first` with the inline-row sentinel in `last`. Duplicates use one-based link records. Sets leave both zero.
    pub(super) first: u64,
    pub(super) last: u64,
}

impl Slot {
    fn decode(bytes: &[u8]) -> Self {
        let word =
            |offset: usize| u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap());
        Self {
            hash: word(0),
            key: word(8),
            first: word(16),
            last: word(24),
        }
    }

    fn encode(self) -> [u8; SLOT_BYTES] {
        let mut bytes = [0; SLOT_BYTES];
        for (out, value) in bytes
            .chunks_exact_mut(8)
            .zip([self.hash, self.key, self.first, self.last])
        {
            out.copy_from_slice(&value.to_le_bytes());
        }
        bytes
    }
}

pub(super) struct Directory {
    file: BlockTemporaryFile<PAGE_BYTES>,
    capacity: u64,
    entries: u64,
    page_number: Option<u64>,
    page: [u8; PAGE_BYTES],
    #[cfg(test)]
    pub(super) probes: u64,
}

impl Directory {
    pub(super) fn new(path: &Path, capacity: u64) -> io::Result<Self> {
        debug_assert!(capacity.is_power_of_two() && capacity >= 8);
        let length = capacity
            .checked_mul(SLOT_BYTES as u64)
            .ok_or_else(|| io::Error::other("spill directory exceeds addressable storage"))?;
        let file = BlockTemporaryFile::new_in(path)?;
        file.set_len(length)?;
        Ok(Self {
            file,
            capacity,
            entries: 0,
            page_number: None,
            page: [0; PAGE_BYTES],
            #[cfg(test)]
            probes: 0,
        })
    }

    fn read(&mut self, index: u64) -> io::Result<Slot> {
        #[cfg(test)]
        {
            self.probes += 1;
        }
        let offset = index * SLOT_BYTES as u64;
        let page = offset / PAGE_BYTES as u64;
        if self.page_number != Some(page) {
            self.page_number = None;
            self.file.seek(SeekFrom::Start(page * PAGE_BYTES as u64))?;
            self.file.read_exact(&mut self.page)?;
            self.page_number = Some(page);
        }
        let start = usize::try_from(offset % PAGE_BYTES as u64).unwrap();
        Ok(Slot::decode(&self.page[start..start + SLOT_BYTES]))
    }

    pub(super) fn find(
        &mut self,
        hash: u64,
        mut matches: impl FnMut(Slot) -> IndexResult<bool>,
    ) -> IndexResult<(u64, Slot)> {
        let mask = self.capacity - 1;
        let mut position = hash & mask;
        for visited in 0..self.capacity {
            if visited % 64 == 0 {
                uqa_sql::expr::check_statement_cancellation()?;
            }
            let slot = self.read(position)?;
            if slot.key == 0 || (slot.hash == hash && matches(slot)?) {
                return Ok((position, slot));
            }
            position = (position + 1) & mask;
        }
        Err(io::Error::other("full spill hash directory").into())
    }

    pub(super) fn needs_growth(&self) -> bool {
        self.entries >= self.capacity - self.capacity / 4
    }

    pub(super) fn grow(&mut self, path: &Path) -> IndexResult<()> {
        let capacity = self
            .capacity
            .checked_mul(2)
            .ok_or_else(|| io::Error::other("spill directory size overflow"))?;
        let mut next = Self::new(path, capacity)?;
        for old in 0..self.capacity {
            if old % 64 == 0 {
                uqa_sql::expr::check_statement_cancellation()?;
            }
            let entry = self.read(old)?;
            if entry.key != 0 {
                let (index, _) = next.find(entry.hash, |_| Ok(false))?;
                next.publish(index, entry, true)?;
            }
        }
        #[cfg(test)]
        {
            next.probes += self.probes;
        }
        *self = next;
        Ok(())
    }

    pub(super) fn publish(&mut self, index: u64, slot: Slot, inserted: bool) -> io::Result<()> {
        let offset = index * SLOT_BYTES as u64;
        let bytes = slot.encode();
        self.file.seek(SeekFrom::Start(offset))?;
        // A complete slot belongs to one authenticated block. Its previous value remains readable on failure.
        self.file.write_all(&bytes)?;
        if self.page_number == Some(offset / PAGE_BYTES as u64) {
            let start = usize::try_from(offset % PAGE_BYTES as u64).unwrap();
            self.page[start..start + SLOT_BYTES].copy_from_slice(&bytes);
        }
        self.entries += u64::from(inserted);
        Ok(())
    }
}
