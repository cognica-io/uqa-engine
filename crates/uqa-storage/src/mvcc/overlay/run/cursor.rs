//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered reads of a run through cached blocks or bounded streaming buffers.

use std::ops::Bound;
use std::sync::Arc;

use crate::mvcc::VersionResult;
use crate::read_control::StorageReadControl;

use super::{entry::RunEntry, reader::EntryReader};
use super::{RunCacheReader, SpilledRun};

/// Visits a run's changes in key order from a starting bound, with bounded decoded entries.
pub(in crate::mvcc) struct RunCursor {
    run: Arc<SpilledRun>,
    next_block: usize,
    block: Option<EntryReader>,
    start: Option<(Vec<u8>, bool)>,
    reader: Option<RunCacheReader>,
}

impl RunCursor {
    pub(super) fn new(run: Arc<SpilledRun>, start: Bound<&[u8]>) -> Self {
        let next_block = run.first_block_from(start);
        let start = match start {
            Bound::Unbounded => None,
            Bound::Included(key) => Some((key.to_vec(), true)),
            Bound::Excluded(key) => Some((key.to_vec(), false)),
        };
        Self {
            reader: Some(run.cache_reader()),
            run,
            next_block,
            block: None,
            start,
        }
    }

    /// The run this cursor reads.
    pub(in crate::mvcc) fn run(&self) -> &Arc<SpilledRun> {
        &self.run
    }

    /// The next change in key order, or `None` after the last.
    pub(in crate::mvcc) fn next(
        &mut self,
        control: &StorageReadControl,
    ) -> VersionResult<Option<RunEntry>> {
        loop {
            control.cancellation().check()?;
            if self.block.is_none() {
                if self.next_block >= self.run.blocks.len() {
                    self.reader = None;
                    return Ok(None);
                }
                self.block = Some(EntryReader::new(&self.run, self.next_block, control)?);
                self.next_block += 1;
            }
            let entry =
                self.block
                    .as_mut()
                    .expect("a loaded block")
                    .next_matching(control, |key| {
                        if let Some((start, inclusive)) = &self.start {
                            if if *inclusive {
                                key < start.as_slice()
                            } else {
                                key <= start.as_slice()
                            } {
                                return false;
                            }
                            self.start = None;
                        }
                        true
                    })?;
            if entry.is_some() {
                return Ok(entry);
            }
            self.block = None;
        }
    }
}
