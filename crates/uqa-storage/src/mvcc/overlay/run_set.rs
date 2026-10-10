//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The spilled runs of a private root and how they merge.

use std::ops::Bound;
use std::sync::Arc;

use triomphe::Arc as StrongArc;
use uqa_core::memory::MemoryBudget;

use crate::mvcc::VersionResult;
use crate::read_control::StorageReadControl;

use super::run::{SpilledRun, SpilledRunWriter};
use super::tiers::{TieredChange, TieredCursor};

/// Runs whose sizes are in one size class merge once this many of them accumulate, which rewrites each change a logarithmic number of times and keeps the number of runs logarithmic in the size of the overlay.
const FAN_IN: usize = 4;

/// The spilled runs of a private root, newest last. Cloning shares them, so a savepoint or a command view keeps the runs it saw while the transaction adds or merges others. A root that never spilled holds no allocation.
#[derive(Clone)]
pub(super) struct RunSet(Option<StrongArc<Vec<Arc<SpilledRun>>>>);

impl RunSet {
    pub(super) const fn empty() -> Self {
        Self(None)
    }

    pub(super) fn from_run(run: Arc<SpilledRun>) -> Self {
        Self(Some(StrongArc::new(vec![run])))
    }

    pub(super) fn is_empty(&self) -> bool {
        self.0.is_none()
    }

    pub(super) fn only_run(&self) -> Option<&Arc<SpilledRun>> {
        match self.runs() {
            [run] => Some(run),
            _ => None,
        }
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.0.as_ref().map_or(0, |runs| runs.len())
    }

    /// Changes and literal key bytes across the runs; overlapping keys remain a safe upper bound for a merge.
    pub(super) fn size(&self) -> (u64, u64) {
        self.runs().iter().fold((0, 0), |(entries, bytes), run| {
            (
                entries.saturating_add(run.len()),
                bytes.saturating_add(run.key_bytes()),
            )
        })
    }

    fn runs(&self) -> &[Arc<SpilledRun>] {
        self.0.as_deref().map_or(&[], Vec::as_slice)
    }

    pub(super) fn newest_first(&self) -> impl Iterator<Item = &Arc<SpilledRun>> {
        self.runs().iter().rev()
    }

    /// The newest spilled change of `key`, without its value.
    pub(super) fn get(
        &self,
        key: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<Option<TieredChange<'static>>> {
        for run in self.newest_first() {
            if let Some(entry) = run.get(key, control)? {
                return Ok(Some(TieredChange::Spilled {
                    entry,
                    run: Arc::clone(run),
                }));
            }
        }
        Ok(None)
    }

    /// The spilled change with the greatest key before `end`, the newest one when several runs hold that key.
    pub(super) fn last_before(
        &self,
        end: Bound<&[u8]>,
        control: &StorageReadControl,
    ) -> VersionResult<Option<TieredChange<'static>>> {
        let mut found: Option<TieredChange<'static>> = None;
        for run in self.newest_first() {
            if let Some(entry) = run.last_before(end, control)? {
                if found
                    .as_ref()
                    .is_none_or(|found| entry.key.bytes() > found.key())
                {
                    found = Some(TieredChange::Spilled {
                        entry,
                        run: Arc::clone(run),
                    });
                }
            }
        }
        Ok(found)
    }

    /// These runs with `run` as the newest, after merging the newest runs of one size class whenever `FAN_IN` of them accumulate. A run of `base` bytes, the size of one spill, is in the smallest class.
    pub(super) fn with_run(
        &self,
        run: SpilledRun,
        base: u64,
        memory: &MemoryBudget,
        control: &StorageReadControl,
    ) -> VersionResult<Self> {
        let class = |run: &SpilledRun| (run.bytes() / base.max(1)).max(1).ilog(FAN_IN as u64);
        let mut runs = self.runs().to_vec();
        runs.push(Arc::new(run));
        loop {
            let count = runs.len();
            let Some(newest) = runs.last().map(|run| class(run)) else {
                break;
            };
            let merging = runs
                .iter()
                .rev()
                .take_while(|run| class(run) <= newest)
                .count();
            if merging < FAN_IN {
                break;
            }
            let merged = merge(&runs[count - merging..], memory, control)?;
            runs.truncate(count - merging);
            runs.push(Arc::new(merged));
        }
        Ok(Self(Some(StrongArc::new(runs))))
    }
}

/// One run with the newest change of each key in `runs`, which are ordered oldest first.
fn merge(
    runs: &[Arc<SpilledRun>],
    memory: &MemoryBudget,
    control: &StorageReadControl,
) -> VersionResult<SpilledRun> {
    let entries = runs.iter().map(|run| run.len()).sum();
    let key_bytes = runs.iter().map(|run| run.key_bytes()).sum();
    let set = RunSet(Some(StrongArc::new(runs.to_vec())));
    // Reader admission must see the writer's retained workspace before it divides the remaining allowance among runs.
    let mut writer = SpilledRunWriter::new(entries, key_bytes, memory)?;
    let mut cursor = TieredCursor::new(None, &set, Bound::Unbounded, control)?;
    while let Some(change) = cursor.next(control)? {
        let TieredChange::Spilled { entry, run } = &change else {
            return Err(crate::mvcc::VersionError::InvalidEncoding(
                "a merge of runs read a memory change",
            ));
        };
        writer.push_spilled(entry, run, control)?;
    }
    Ok(writer
        .finish()?
        .expect("merging nonempty runs yields a nonempty run"))
}

#[cfg(test)]
mod tests;
