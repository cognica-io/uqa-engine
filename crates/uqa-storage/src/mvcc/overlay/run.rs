//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable sorted runs of private record changes on encrypted temporary files.

mod cache;
mod cursor;
mod entry;
mod filter;
#[cfg(test)]
pub(in crate::mvcc) mod read_counts;
mod reader;
#[cfg(test)]
mod tests;
#[cfg(test)]
pub(in crate::mvcc) mod write_counts;
mod writer;

pub(in crate::mvcc) use cache::RunCacheReader;
pub(in crate::mvcc) use cursor::RunCursor;
pub(in crate::mvcc) use entry::RunEntry;
pub(in crate::mvcc) use writer::SpilledRunWriter;

use std::io::{Read, Seek, SeekFrom};
use std::ops::Bound;
use std::sync::Arc;

use uqa_core::memory::{BudgetedVec, MemoryBudget};

use crate::read_control::StorageReadControl;
use crate::temporary_file::TemporaryFile;

use crate::mvcc::key::RecordKey;
use crate::mvcc::{VersionError, VersionResult};
use cache::RunCache;
use entry::ValueLocation;
use filter::KeyFilter;

/// The aligned span of the value file a read of a small value decrypts and keeps.
const VALUE_CHUNK: u64 = 16 * 1024;

/// One block of a run's entry file: its complete key interval and byte range.
struct RunBlock {
    first: RecordKey,
    last: RecordKey,
    offset: u64,
    end: u64,
}

/// An immutable, key-ordered run of private changes in two encrypted temporary files: the entries, whose blocks a bounded in-memory index locates, and their values, which a read loads only when it needs them. A key filter answers most lookups of keys the run lacks without reading it. The files are removed with the last handle to the run.
pub(in crate::mvcc) struct SpilledRun {
    entries: TemporaryFile,
    values: TemporaryFile,
    blocks: BudgetedVec<RunBlock>,
    filter: Option<KeyFilter>,
    prefix_filter: Option<KeyFilter>,
    last: RecordKey,
    len: u64,
    key_bytes: u64,
    entry_bytes: u64,
    bytes: u64,
    /// One bit for each write kind code the run holds.
    kinds: u16,
    cache: Arc<RunCache>,
}

impl SpilledRun {
    pub(in crate::mvcc) fn shares_allowance(&self, memory: &MemoryBudget) -> bool {
        self.cache.memory().shares_allowance(memory)
    }

    /// Share decoded blocks while a lookup operation is active, without tying their lifetime to this run.
    pub(in crate::mvcc) fn cache_reader(&self) -> RunCacheReader {
        self.cache.reader()
    }

    /// Whether the run holds a write of `kind`.
    pub(in crate::mvcc) fn has_kind(&self, kind: crate::mvcc::commit::RecordWriteKind) -> bool {
        self.kinds & (1 << kind.code()) != 0
    }

    /// Whether some write of the run is not canonical.
    pub(in crate::mvcc) fn typed(&self) -> bool {
        self.kinds & !(1 << crate::mvcc::commit::RecordWriteKind::Canonical.code()) != 0
    }

    /// The number of changes in the run.
    pub(in crate::mvcc) fn len(&self) -> u64 {
        self.len
    }

    /// Literal key bytes, excluding entry metadata and values, for sizing derived filters.
    pub(in crate::mvcc) fn key_bytes(&self) -> u64 {
        self.key_bytes
    }

    /// The bytes of the run's files, which decide which runs merge.
    pub(super) fn bytes(&self) -> u64 {
        self.bytes
    }

    /// The change of `key`, if the run has one.
    pub(in crate::mvcc) fn get(
        &self,
        key: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<Option<RunEntry>> {
        control.cancellation().check()?;
        if key > self.last.bytes()
            || self
                .filter
                .as_ref()
                .is_some_and(|filter| !filter.may_contain(key))
        {
            return Ok(None);
        }
        let Some(index) = self.block_at_or_before(key) else {
            return Ok(None);
        };
        if key > self.blocks[index].last.bytes() {
            return Ok(None);
        }
        let mut block = reader::EntryReader::new(self, index, control)?;
        block.select(control, |candidate| match candidate.cmp(key) {
            std::cmp::Ordering::Less => reader::Selection::Skip,
            std::cmp::Ordering::Equal => reader::Selection::Take,
            std::cmp::Ordering::Greater => reader::Selection::End,
        })
    }

    /// The change with the greatest key before `end`, if the run has one.
    pub(super) fn last_before(
        &self,
        end: Bound<&[u8]>,
        control: &StorageReadControl,
    ) -> VersionResult<Option<RunEntry>> {
        control.cancellation().check()?;
        let before = |key: &[u8]| match end {
            Bound::Included(end) => key <= end,
            Bound::Excluded(end) => key < end,
            Bound::Unbounded => true,
        };
        let mut index = self
            .blocks
            .partition_point(|block| before(block.first.bytes()));
        while index > 0 {
            index -= 1;
            let mut block = reader::EntryReader::new(self, index, control)?;
            let mut found = None;
            while let Some(entry) = block.select(control, |key| {
                if before(key) {
                    reader::Selection::Take
                } else {
                    reader::Selection::End
                }
            })? {
                found = Some(entry);
            }
            if found.is_some() {
                return Ok(found);
            }
        }
        Ok(None)
    }

    /// Visit the run's changes in key order from `start`.
    pub(in crate::mvcc) fn cursor(self: &Arc<Self>, start: Bound<&[u8]>) -> RunCursor {
        RunCursor::new(Arc::clone(self), start)
    }

    /// Whether the run can contain this literal prefix. Interval bounds and an optional false-positive-only membership filter reject absent prefixes without an entry reader or decoded block.
    pub(super) fn intersects_prefix(&self, prefix: &[u8]) -> bool {
        let index = self
            .blocks
            .partition_point(|block| block.last.bytes() < prefix);
        self.blocks.get(index).is_some_and(|block| {
            (block.first.bytes() <= prefix || block.first.bytes().starts_with(prefix))
                && (prefix.is_empty()
                    || self
                        .prefix_filter
                        .as_ref()
                        .is_none_or(|filter| filter.may_contain(prefix)))
        })
    }

    /// Pass the value at `location` to `sink` in chunks, without holding it whole. A value within one aligned chunk of the value file is passed from that chunk, which the run keeps for the values that follow it.
    pub(in crate::mvcc) fn copy_value(
        &self,
        location: ValueLocation,
        sink: &mut dyn FnMut(&[u8]) -> VersionResult<()>,
        control: &StorageReadControl,
    ) -> VersionResult<()> {
        if let Some((chunk, begin)) = self.value_chunk(location, control)? {
            return sink(&chunk[begin..begin + location.len as usize]);
        }
        let mut reader = self.values.reopen().map_err(spill_error)?;
        reader
            .seek(SeekFrom::Start(location.offset))
            .map_err(spill_error)?;
        let mut chunk = [0_u8; VALUE_CHUNK as usize];
        let mut remaining = location.len;
        while remaining > 0 {
            let count = remaining.min(chunk.len() as u64) as usize;
            let count = read_chunk(&mut reader, &mut chunk[..count], control)?;
            sink(&chunk[..count])?;
            remaining -= count as u64;
        }
        Ok(())
    }

    /// The value at `location`, charged to `control`. A value within one aligned chunk of the value file is copied from that chunk, which the run keeps for the reads of the values that follow it.
    pub(in crate::mvcc) fn load_value(
        &self,
        location: ValueLocation,
        control: &StorageReadControl,
    ) -> VersionResult<Arc<BudgetedVec<u8>>> {
        #[cfg(test)]
        read_counts::value();
        let mut value = BudgetedVec::new(control.memory());
        if let Some((chunk, begin)) = self.value_chunk(location, control)? {
            value.extend_from_slice(&chunk[begin..begin + location.len as usize])?;
            return Ok(Arc::new(value));
        }
        read_range(
            &self.values,
            location.offset,
            location.len,
            control.memory(),
            control,
        )
        .map(Arc::new)
    }

    /// The aligned chunk of the value file that holds the whole value at `location`, and the value's offset in it; `None` for a value that crosses a chunk boundary, or when the run's allowance cannot hold a chunk it does not already keep.
    fn value_chunk(
        &self,
        location: ValueLocation,
        control: &StorageReadControl,
    ) -> VersionResult<Option<(Arc<BudgetedVec<u8>>, usize)>> {
        let start = location.offset - location.offset % VALUE_CHUNK;
        if location.offset + location.len > start + VALUE_CHUNK {
            return Ok(None);
        }
        let begin = (location.offset - start) as usize;
        if let Some(chunk) = self.cache.chunk(start) {
            return Ok(Some((chunk, begin)));
        }
        let end = (start + VALUE_CHUNK).min(self.bytes - self.entry_bytes);
        // Cache admission must preserve workspace for the value copy and the other active readers, even when the run and consumer share one allowance.
        let available = self
            .cache
            .memory()
            .available()
            .min(control.memory().available());
        if end - start > (available / 2) as u64 {
            return Ok(None);
        }
        match read_range(
            &self.values,
            start,
            end - start,
            self.cache.memory(),
            control,
        ) {
            Ok(bytes) => {
                let bytes = Arc::new(bytes);
                self.cache.keep_chunk(start, &bytes);
                Ok(Some((bytes, begin)))
            }
            Err(VersionError::Memory(_)) => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// The index of the last block whose first key is at most `key`.
    fn block_at_or_before(&self, key: &[u8]) -> Option<usize> {
        self.blocks
            .partition_point(|block| block.first.bytes() <= key)
            .checked_sub(1)
    }

    /// The first block that may hold a key in `start`'s range, or the block count when the range is beyond this run.
    fn first_block_from(&self, start: Bound<&[u8]>) -> usize {
        match start {
            Bound::Unbounded => 0,
            Bound::Included(key) => self
                .blocks
                .partition_point(|block| block.last.bytes() < key),
            Bound::Excluded(key) => self
                .blocks
                .partition_point(|block| block.last.bytes() <= key),
        }
    }

    /// The bytes of entry block `index`, which the run keeps for the reads that follow; when its allowance cannot keep them, they are charged to `control` instead.
    fn read_block(
        &self,
        index: usize,
        control: &StorageReadControl,
    ) -> VersionResult<Arc<BudgetedVec<u8>>> {
        if let Some(block) = self.cache.block(index) {
            return Ok(block);
        }
        #[cfg(test)]
        read_counts::block();
        let block = &self.blocks[index];
        let len = block.end - block.offset;
        match read_range(
            &self.entries,
            block.offset,
            len,
            self.cache.memory(),
            control,
        ) {
            Ok(bytes) => {
                let bytes = Arc::new(bytes);
                self.cache.keep_block(index, &bytes);
                Ok(bytes)
            }
            Err(VersionError::Memory(_)) => {
                read_range(&self.entries, block.offset, len, control.memory(), control)
                    .map(Arc::new)
            }
            Err(error) => Err(error),
        }
    }
}

/// The `len` bytes at `offset` of `file`, charged to `memory`.
fn read_range(
    file: &TemporaryFile,
    offset: u64,
    len: u64,
    memory: &MemoryBudget,
    control: &StorageReadControl,
) -> VersionResult<BudgetedVec<u8>> {
    control.cancellation().check()?;
    let len = usize::try_from(len)
        .map_err(|_| VersionError::InvalidEncoding("spilled range exceeds the address space"))?;
    let mut bytes = BudgetedVec::new(memory);
    bytes.reserve(len)?;
    let mut reader = file.reopen().map_err(spill_error)?;
    reader.seek(SeekFrom::Start(offset)).map_err(spill_error)?;
    let mut chunk = [0_u8; 16 * 1024];
    while bytes.len() < len {
        let count = (len - bytes.len()).min(chunk.len());
        let count = read_chunk(&mut reader, &mut chunk[..count], control)?;
        bytes.extend_from_slice(&chunk[..count])?;
    }
    Ok(bytes)
}

/// Consume a physical block's short read before requesting another block. Filling an unaligned logical chunk with `read_exact` would decrypt its trailing block again on the next iteration.
fn read_chunk(
    reader: &mut TemporaryFile,
    output: &mut [u8],
    control: &StorageReadControl,
) -> VersionResult<usize> {
    loop {
        control.cancellation().check()?;
        match reader.read(output) {
            Ok(0) => {
                return Err(spill_error(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "failed to fill whole buffer",
                )))
            }
            Ok(count) => return Ok(count),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => (),
            Err(error) => return Err(spill_error(error)),
        }
    }
}

/// A failed read or write of a spill file.
pub(super) fn spill_error(error: std::io::Error) -> VersionError {
    crate::StorageBackendError::backend("transaction spill file", error).into()
}
