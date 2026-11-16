//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Pending work retains its admitted stack capacity and owns every suspended frame.

use super::Result;
use uqa_core::memory::{BudgetedVec, ProductionControl};

enum Storage<T> {
    Ordinary(Vec<T>),
    Admitted(BudgetedVec<T>),
}

pub(crate) struct Stack<T, const N: usize = 0> {
    inline: [Option<T>; N],
    len: usize,
    storage: Storage<T>,
}

impl<T, const N: usize> Stack<T, N> {
    pub(crate) fn new(control: ProductionControl<'_>) -> Self {
        Self {
            inline: std::array::from_fn(|_| None),
            len: 0,
            storage: control.budget().map_or_else(
                || Storage::Ordinary(Vec::new()),
                |budget| Storage::Admitted(BudgetedVec::new(budget)),
            ),
        }
    }
    pub(crate) fn push(&mut self, task: T) -> Result<()> {
        if self.len < N {
            self.inline[self.len] = Some(task);
            self.len += 1;
            return Ok(());
        }
        match &mut self.storage {
            Storage::Ordinary(tasks) => {
                tasks
                    .try_reserve(1)
                    .map_err(uqa_core::memory::MemoryError::from)?;
                tasks.push(task);
            }
            Storage::Admitted(tasks) => tasks.push(task)?,
        }
        Ok(())
    }
    pub(crate) fn last_mut(&mut self) -> Option<&mut T> {
        let value = match &mut self.storage {
            Storage::Ordinary(tasks) => tasks.last_mut(),
            Storage::Admitted(tasks) => tasks.last_mut(),
        };
        value.or_else(|| {
            self.len
                .checked_sub(1)
                .and_then(|last| self.inline[last].as_mut())
        })
    }
    pub(crate) fn pop(&mut self) -> Option<T> {
        let value = match &mut self.storage {
            Storage::Ordinary(tasks) => tasks.pop(),
            Storage::Admitted(tasks) => tasks.pop(),
        };
        value.or_else(|| {
            self.len = self.len.checked_sub(1)?;
            self.inline[self.len].take()
        })
    }
}
