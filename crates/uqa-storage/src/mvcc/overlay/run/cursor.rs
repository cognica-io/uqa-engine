//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered reads of a run, one block at a time.

use std::ops::Bound;
use std::sync::Arc;

use uqa_core::memory::BudgetedVec;

use crate::mvcc::VersionResult;
use crate::read_control::StorageReadControl;

use super::entry::{self, RunEntry};
use super::{RunCacheReader, SpilledRun};

/// Visits a run's changes in key order from a starting bound, holding one block of entries at a time.
pub(in crate::mvcc) struct RunCursor {
    run: Arc<SpilledRun>,
    next_block: usize,
    block: Option<Arc<BudgetedVec<u8>>>,
    position: usize,
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
            position: 0,
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
            if self
                .block
                .as_ref()
                .is_none_or(|block| self.position >= block.len())
            {
                self.block = None;
                if self.next_block >= self.run.blocks.len() {
                    self.reader = None;
                    return Ok(None);
                }
                self.block = Some(self.run.read_block(self.next_block, control)?);
                self.next_block += 1;
                self.position = 0;
                continue;
            }
            let block = self.block.as_ref().expect("a loaded block");
            let raw = entry::decode(block, &mut self.position)?;
            if let Some((start, inclusive)) = &self.start {
                let before = if *inclusive {
                    raw.key < start.as_slice()
                } else {
                    raw.key <= start.as_slice()
                };
                if before {
                    continue;
                }
                self.start = None;
            }
            return raw.owned(control.memory()).map(Some);
        }
    }
}
