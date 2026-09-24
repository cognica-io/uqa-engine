//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Budgeted private identity selection preserves the last evaluated change per row.

use uqa_core::{memory::BudgetedVec, DocId};
use uqa_storage::{read_control::StorageReadControl, StorageBackendResult};

pub struct DocumentSelection {
    rows: BudgetedVec<(DocId, bool, usize)>,
}

impl DocumentSelection {
    pub fn new(control: &StorageReadControl) -> Self {
        Self {
            rows: BudgetedVec::new(control.memory()),
        }
    }

    pub fn insert(
        &mut self,
        id: DocId,
        present: bool,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        control.check()?;
        self.rows.push((id, present, self.rows.len()))?;
        Ok(())
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub(super) fn finish(mut self, control: &StorageReadControl) -> StorageBackendResult<Self> {
        control.check()?;
        self.rows
            .sort_unstable_by_key(|(id, _, order)| (*id, *order));
        let mut write = 0;
        for read in 0..self.rows.len() {
            control.check()?;
            let row = self.rows[read];
            if write != 0 && self.rows[write - 1].0 == row.0 {
                self.rows[write - 1] = row;
            } else {
                self.rows[write] = row;
                write += 1;
            }
        }
        self.rows.truncate(write);
        Ok(self)
    }

    pub(super) fn entries(&self) -> impl Iterator<Item = (DocId, bool)> + '_ {
        self.rows.iter().map(|(id, present, _)| (*id, *present))
    }
}
