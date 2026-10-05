//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Entry reads retain a cached block or one bounded streaming buffer under pressure.

use std::{
    io::{BufReader, Seek, SeekFrom},
    sync::Arc,
};
use uqa_core::memory::{BudgetedVec, MemoryError, MemoryReservation};

use crate::{
    mvcc::{VersionError, VersionResult},
    read_control::StorageReadControl,
    temporary_file::TemporaryFile,
};

use super::{
    entry::{self, RunEntry},
    spill_error, SpilledRun,
};

const STREAM_BYTES: usize = 1024;

pub(super) enum Selection {
    Skip,
    Take,
    End,
}

pub(super) enum EntryReader {
    Cached {
        bytes: Arc<BudgetedVec<u8>>,
        position: usize,
    },
    Streaming(Box<StreamingReader>),
}

pub(super) struct StreamingReader {
    reader: BufReader<TemporaryFile>,
    remaining: u64,
    _memory: MemoryReservation,
}

impl EntryReader {
    pub(super) fn new(
        run: &SpilledRun,
        index: usize,
        control: &StorageReadControl,
    ) -> VersionResult<Self> {
        Self::with_block_limit(run, index, usize::MAX, control)
    }

    pub(super) fn with_block_limit(
        run: &SpilledRun,
        index: usize,
        maximum: usize,
        control: &StorageReadControl,
    ) -> VersionResult<Self> {
        control.check()?;
        if let Some(bytes) = run.cache.block(index) {
            return Ok(Self::Cached { bytes, position: 0 });
        }
        run.cache.discard_other_block(index);
        let maximum = maximum.min(control.memory().available() / 2);
        let block = &run.blocks[index];
        if block.end - block.offset <= maximum as u64 {
            match run.read_block(index, control) {
                Ok(bytes) => return Ok(Self::Cached { bytes, position: 0 }),
                Err(VersionError::Memory(MemoryError::Limit { .. })) => (),
                Err(error) => return Err(error),
            }
        }
        let capacity = maximum
            .saturating_sub(size_of::<StreamingReader>())
            .clamp(1, STREAM_BYTES);
        let mut memory = control
            .memory()
            .reserve(size_of::<StreamingReader>() + capacity)?;
        let mut file = run.entries.reopen().map_err(spill_error)?;
        file.seek(SeekFrom::Start(block.offset))
            .map_err(spill_error)?;
        let reader = BufReader::with_capacity(capacity, file);
        memory.grow(reader.capacity() - capacity)?;
        Ok(Self::Streaming(Box::new(StreamingReader {
            reader,
            remaining: block.end - block.offset,
            _memory: memory,
        })))
    }

    pub(super) fn next_matching(
        &mut self,
        control: &StorageReadControl,
        mut matches: impl FnMut(&[u8]) -> bool,
    ) -> VersionResult<Option<RunEntry>> {
        self.select(control, |key| {
            if matches(key) {
                Selection::Take
            } else {
                Selection::Skip
            }
        })
    }

    pub(super) fn select(
        &mut self,
        control: &StorageReadControl,
        mut selection: impl FnMut(&[u8]) -> Selection,
    ) -> VersionResult<Option<RunEntry>> {
        loop {
            control.check()?;
            match self {
                Self::Cached { bytes, position } => {
                    if *position == bytes.len() {
                        return Ok(None);
                    }
                    let raw = entry::decode(bytes, position)?;
                    #[cfg(test)]
                    super::read_counts::entry();
                    match selection(raw.key) {
                        Selection::Take => return raw.owned(control.memory()).map(Some),
                        Selection::End => return Ok(None),
                        Selection::Skip => (),
                    }
                }
                Self::Streaming(stream) => {
                    if stream.remaining == 0 {
                        return Ok(None);
                    }
                    let entry = entry::read(&mut stream.reader, &mut stream.remaining, control)?;
                    #[cfg(test)]
                    super::read_counts::entry();
                    match selection(entry.key.bytes()) {
                        Selection::Take => return Ok(Some(entry)),
                        Selection::End => return Ok(None),
                        Selection::Skip => (),
                    }
                }
            }
        }
    }
}
