//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Explicit point-read lifetimes share decoded immutable blocks without changing record visibility.

use uqa_core::memory::BudgetedVec;

use crate::mvcc::VersionResult;
use crate::read_control::StorageReadControl;

use super::{run::RunCacheReader, run_set::RunSet, PrivateRecordSnapshot};

/// Keeps the existing run caches active across related point lookups. Cached bytes remain charged to their owning allowance; the last reader releases them even if an immutable snapshot or savepoint still retains the run. This scope owns no record view and does not pin returned values.
pub struct PrivateRecordReadScope {
    _readers: BudgetedVec<RunCacheReader>,
}

impl PrivateRecordReadScope {
    pub(super) fn new(runs: &RunSet, control: &StorageReadControl) -> VersionResult<Self> {
        control.check()?;
        let mut readers = BudgetedVec::new(control.memory());
        for run in runs.newest_first() {
            readers.push(run.cache_reader())?;
        }
        Ok(Self { _readers: readers })
    }
}

impl PrivateRecordSnapshot {
    /// Share decoded blocks across point reads of this immutable view. Retaining the view alone does not retain decoded blocks; dropping the last active scope releases them. Existing read methods retain their cancellation and allowance checks.
    pub fn read_scope(
        &self,
        control: &StorageReadControl,
    ) -> VersionResult<PrivateRecordReadScope> {
        PrivateRecordReadScope::new(&self.runs, control)
    }
}

#[cfg(test)]
mod tests;
