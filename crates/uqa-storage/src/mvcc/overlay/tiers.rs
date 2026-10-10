//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered reads that merge the memory tier of a private root with its spilled runs.

mod change;

pub(super) use change::TieredChange;

use std::iter::Peekable;
use std::ops::Bound;

use uqa_core::memory::BudgetedSharedMapIter;

use crate::mvcc::VersionResult;
use crate::read_control::StorageReadControl;

use super::run::{RunCursor, RunEntry};
use super::run_set::RunSet;
use super::Change;
use crate::mvcc::key::RecordKey;

/// The newest change of `key` in `records` or `runs`; the memory tier shadows the runs.
pub(super) fn lookup<'a>(
    records: &'a super::Records,
    runs: &RunSet,
    key: &[u8],
    control: &StorageReadControl,
) -> VersionResult<Option<TieredChange<'a>>> {
    if let Some(change) = records.get(key) {
        return Ok(Some(TieredChange::Resident(change)));
    }
    runs.get(key, control)
}

struct RunHead {
    cursor: RunCursor,
    head: Option<RunEntry>,
}

/// Visits the changes of a private root in key order: its memory tier first, then its runs from newest to oldest, so that only the newest change of each key is returned.
pub(super) struct TieredCursor<'a> {
    records: Option<&'a super::Records>,
    memtable: Option<Peekable<BudgetedSharedMapIter<'a, RecordKey, Change>>>,
    runs: Vec<RunHead>,
}

impl<'a> TieredCursor<'a> {
    /// A cursor over `records` and `runs` from `start`; without `records`, only the runs are read.
    pub(super) fn new(
        records: Option<&'a super::Records>,
        runs: &RunSet,
        start: Bound<&[u8]>,
        control: &StorageReadControl,
    ) -> VersionResult<Self> {
        Self::for_prefix(records, runs, b"", start, control)
    }

    /// Start a prefix scan without opening runs whose complete key interval is disjoint. The consumer still enforces the prefix on each returned change.
    pub(super) fn for_prefix(
        records: Option<&'a super::Records>,
        runs: &RunSet,
        prefix: &[u8],
        start: Bound<&[u8]>,
        control: &StorageReadControl,
    ) -> VersionResult<Self> {
        control.check()?;
        let memtable = records.map(|records| records.range_from::<[u8]>(start).peekable());
        let mut heads = Vec::new();
        // Every run retains a reader concurrently. Leave half the shared workspace for decoded heads, returned values and the consumer instead of letting the first runs take full blocks.
        let block_limit = control.memory().available() / 2 / runs.newest_first().count().max(1);
        for run in runs.newest_first() {
            if !run.intersects_prefix(prefix) {
                continue;
            }
            let mut cursor = run.cursor(start).with_block_limit(block_limit);
            let head = cursor.next(control)?;
            heads.push(RunHead { cursor, head });
        }
        Ok(Self {
            records,
            memtable,
            runs: heads,
        })
    }

    /// Continue at or beyond this key without decoding every intervening change. Heads already beyond the bound retain their decoded entry and readers.
    pub(super) fn seek_to(
        &mut self,
        key: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<Option<TieredChange<'a>>> {
        control.check()?;
        if self
            .memtable
            .as_mut()
            .and_then(Peekable::peek)
            .is_some_and(|(current, _)| current.bytes() < key)
        {
            self.memtable = self
                .records
                .map(|records| records.range_from::<[u8]>(Bound::Included(key)).peekable());
        }
        for run in &mut self.runs {
            if run.head.as_ref().is_some_and(|head| head.key.bytes() < key) {
                run.head = run.cursor.seek_to(key, control)?;
            }
        }
        self.next(control)
    }

    /// The next key's newest change, or `None` after the last key.
    pub(super) fn next(
        &mut self,
        control: &StorageReadControl,
    ) -> VersionResult<Option<TieredChange<'a>>> {
        control.cancellation().check()?;
        // The source of the least key; the memory tier and newer runs come first, so they win ties.
        let mut winner: Option<usize> = None;
        {
            let mut least: Option<&[u8]> = self
                .memtable
                .as_mut()
                .and_then(Peekable::peek)
                .map(|(key, _)| key.bytes());
            if least.is_some() {
                winner = Some(0);
            }
            for (index, run) in self.runs.iter().enumerate() {
                if let Some(head) = &run.head {
                    if least.is_none_or(|least| head.key.bytes() < least) {
                        least = Some(head.key.bytes());
                        winner = Some(index + 1);
                    }
                }
            }
        }
        let Some(winner) = winner else {
            return Ok(None);
        };
        let change = match winner {
            0 => {
                let (_, change) = self
                    .memtable
                    .as_mut()
                    .and_then(Iterator::next)
                    .expect("peeked memory change");
                TieredChange::Resident(change)
            }
            index => {
                let run = &mut self.runs[index - 1];
                let entry = run.head.take().expect("least run head");
                run.head = run.cursor.next(control)?;
                TieredChange::Spilled {
                    entry,
                    run: std::sync::Arc::clone(run.cursor.run()),
                }
            }
        };
        // Older changes of the same key are shadowed.
        for run in &mut self.runs {
            if run
                .head
                .as_ref()
                .is_some_and(|head| head.key.bytes() == change.key())
            {
                run.head = run.cursor.next(control)?;
            }
        }
        Ok(Some(change))
    }
}
