//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Write the changes of a sorted source into a new run.

use std::io::Write;

use uqa_core::memory::{BudgetedVec, MemoryBudget};

use crate::mvcc::commit::RecordWriteKind;
use crate::mvcc::key::RecordKey;
use crate::mvcc::{CommitSequence, PrivateRecordRevision, VersionError, VersionResult};
use crate::read_control::StorageReadControl;
use crate::temporary_file::TemporaryFile;

use super::entry::{self, RunEntry, ValueLocation};
use super::filter::KeyFilter;
use super::{spill_error, RunBlock, SpilledRun};

/// The bytes a writer gathers before it hands a whole block to the encrypted file, which rewrites a partially written block on every write.
const FLUSH_BYTES: usize = 16 * 1024;
/// The fewest entry bytes in a block.
const MIN_BLOCK_BYTES: u64 = 16 * 1024;
/// The most blocks a run's index holds; a larger run has larger blocks, so its index stays within a fixed allowance.
const MAX_BLOCKS: u64 = 4096;

struct BufferedFile {
    file: TemporaryFile,
    buffer: BudgetedVec<u8>,
    flush_bytes: usize,
    written: u64,
}

impl BufferedFile {
    fn new(memory: &MemoryBudget, flush_bytes: usize) -> VersionResult<Self> {
        let mut buffer = BudgetedVec::new(memory);
        buffer.reserve(flush_bytes)?;
        Ok(Self {
            file: TemporaryFile::new().map_err(spill_error)?,
            buffer,
            flush_bytes,
            written: 0,
        })
    }

    fn position(&self) -> u64 {
        self.written + self.buffer.len() as u64
    }

    fn append(&mut self, mut bytes: &[u8]) -> VersionResult<()> {
        while !bytes.is_empty() {
            let count = bytes.len().min(self.flush_bytes - self.buffer.len());
            self.buffer.extend_from_slice(&bytes[..count])?;
            bytes = &bytes[count..];
            if self.buffer.len() == self.flush_bytes {
                self.flush()?;
            }
        }
        Ok(())
    }

    fn flush(&mut self) -> VersionResult<()> {
        if self.buffer.is_empty() {
            return Ok(());
        }
        self.file.write_all(&self.buffer).map_err(spill_error)?;
        self.written += self.buffer.len() as u64;
        self.buffer.clear();
        Ok(())
    }
}

/// Builds a run from changes supplied in strictly increasing key order.
pub(in crate::mvcc) struct SpilledRunWriter {
    entries: BufferedFile,
    values: BufferedFile,
    encoded: Vec<u8>,
    blocks: BudgetedVec<RunBlock>,
    block_bytes: u64,
    filter: Option<KeyFilter>,
    last: Option<RecordKey>,
    len: u64,
    kinds: u16,
    memory: MemoryBudget,
}

impl SpilledRunWriter {
    /// A writer for about `entries` changes whose keys hold about `key_bytes` bytes in all; the estimates size the block index and the key filter. The index and the filter are charged to `memory`, and the filter is left out when `memory` cannot hold it.
    pub(in crate::mvcc) fn new(
        entries: u64,
        key_bytes: u64,
        memory: &MemoryBudget,
    ) -> VersionResult<Self> {
        let encoded = key_bytes
            .saturating_add(entries.saturating_mul(entry::encoded_len(0, true, true) as u64));
        // Keep the resident input intact until publication and leave half the available workspace for the run index and merge readers.
        let flush_bytes = (memory.available() / 4).clamp(1, FLUSH_BYTES);
        Ok(Self {
            entries: BufferedFile::new(memory, flush_bytes)?,
            values: BufferedFile::new(memory, flush_bytes)?,
            encoded: Vec::new(),
            blocks: BudgetedVec::new(memory),
            block_bytes: (encoded / MAX_BLOCKS).max(MIN_BLOCK_BYTES),
            filter: KeyFilter::with_capacity(entries, memory),
            last: None,
            len: 0,
            kinds: 0,
            memory: memory.clone(),
        })
    }

    /// Append one change; its key must follow every key appended before it.
    pub(in crate::mvcc) fn push(
        &mut self,
        key: &[u8],
        expected: Option<CommitSequence>,
        kind: RecordWriteKind,
        identity: PrivateRecordRevision,
        value: Option<&[u8]>,
        control: &StorageReadControl,
    ) -> VersionResult<()> {
        self.begin(key, control)?;
        let value = match value {
            Some(bytes) => {
                let location = ValueLocation {
                    offset: self.values.position(),
                    len: bytes.len() as u64,
                };
                self.values.append(bytes)?;
                #[cfg(test)]
                super::write_counts::value(bytes.len() as u64, false);
                Some(location)
            }
            None => None,
        };
        self.end(key, expected, kind, identity, value)
    }

    /// Append a change of another run, copying its value between the runs' files in chunks; its key must follow every key appended before it.
    pub(in crate::mvcc) fn push_spilled(
        &mut self,
        entry: &RunEntry,
        source: &SpilledRun,
        control: &StorageReadControl,
    ) -> VersionResult<()> {
        let key = entry.key.bytes();
        self.begin(key, control)?;
        let value = match entry.value {
            Some(location) => {
                let copied = ValueLocation {
                    offset: self.values.position(),
                    len: location.len,
                };
                let values = &mut self.values;
                source.copy_value(location, &mut |chunk| values.append(chunk), control)?;
                #[cfg(test)]
                super::write_counts::value(location.len, true);
                Some(copied)
            }
            None => None,
        };
        self.end(key, entry.expected, entry.kind, entry.identity, value)
    }

    /// Check the order of `key` and start a block for it when the current block is full.
    fn begin(&mut self, key: &[u8], control: &StorageReadControl) -> VersionResult<()> {
        control.cancellation().check()?;
        if self.last.as_ref().is_some_and(|last| last.bytes() >= key) {
            return Err(VersionError::InvalidEncoding(
                "spilled records are not in increasing key order",
            ));
        }
        let position = self.entries.position();
        let starts_block = self
            .blocks
            .last()
            .is_none_or(|block: &RunBlock| position - block.offset >= self.block_bytes);
        if starts_block {
            if let Some(block) = self.blocks.last_mut() {
                block.end = position;
                block.last = self
                    .last
                    .as_ref()
                    .expect("preceding block has entries")
                    .clone();
            }
            let first = RecordKey::new(key, &self.memory)?;
            self.blocks.push(RunBlock {
                last: first.clone(),
                first,
                offset: position,
                end: position,
            })?;
        }
        Ok(())
    }

    /// Append the entry of `key`, whose value, if it has one, is at `value` in the value file.
    fn end(
        &mut self,
        key: &[u8],
        expected: Option<CommitSequence>,
        kind: RecordWriteKind,
        identity: PrivateRecordRevision,
        value: Option<ValueLocation>,
    ) -> VersionResult<()> {
        self.encoded.clear();
        entry::encode(&mut self.encoded, key, expected, kind, identity, value)?;
        self.kinds |= 1 << kind.code();
        self.entries.append(&self.encoded)?;
        if let Some(filter) = &mut self.filter {
            filter.insert(key);
        }
        self.last = Some(RecordKey::new(key, &self.memory)?);
        self.len += 1;
        Ok(())
    }

    /// The run of the appended changes, or `None` when none was appended.
    pub(in crate::mvcc) fn finish(mut self) -> VersionResult<Option<SpilledRun>> {
        let Some(last) = self.last.take() else {
            return Ok(None);
        };
        self.entries.flush()?;
        self.values.flush()?;
        let end = self.entries.position();
        if let Some(block) = self.blocks.last_mut() {
            block.end = end;
            block.last = last.clone();
        }
        Ok(Some(SpilledRun {
            entry_bytes: end,
            bytes: end + self.values.position(),
            entries: self.entries.file,
            values: self.values.file,
            blocks: self.blocks,
            filter: self.filter,
            last,
            len: self.len,
            kinds: self.kinds,
            cache: std::sync::Arc::new(super::cache::RunCache::new(&self.memory)),
        }))
    }
}
