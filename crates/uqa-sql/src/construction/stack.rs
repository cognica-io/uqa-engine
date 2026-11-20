//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Pending work retains its admitted stack capacity and owns every suspended frame.

use super::Result;
use uqa_core::memory::{BudgetedVec, MemoryBudget, MemoryError, ProductionControl};

#[derive(Debug)]
enum Storage<T> {
    Ordinary(Vec<T>),
    Admitted(BudgetedVec<T>),
}

#[derive(Debug)]
pub(crate) struct Stack<T, const N: usize = 0> {
    inline: [Option<T>; N],
    len: usize,
    storage: Storage<T>,
}

impl<T, const N: usize> Stack<T, N> {
    pub(crate) fn new(control: ProductionControl<'_>) -> Self {
        Self::with_budget(control.budget())
    }
    pub(crate) fn with_budget(budget: Option<&MemoryBudget>) -> Self {
        Self {
            inline: std::array::from_fn(|_| None),
            len: 0,
            storage: budget.map_or_else(
                || Storage::Ordinary(Vec::new()),
                |budget| Storage::Admitted(BudgetedVec::new(budget)),
            ),
        }
    }
    pub(crate) fn len(&self) -> usize {
        self.len
            + match &self.storage {
                Storage::Ordinary(values) => values.len(),
                Storage::Admitted(values) => values.len(),
            }
    }
    pub(crate) fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub(crate) fn capacity(&self) -> usize {
        N + match &self.storage {
            Storage::Ordinary(values) => values.capacity(),
            Storage::Admitted(values) => values.capacity(),
        }
    }
    pub(crate) fn reserve(&mut self, additional: usize) -> Result<()> {
        let required = self
            .len()
            .checked_add(additional)
            .ok_or(MemoryError::SizeOverflow)?
            .saturating_sub(N);
        match &mut self.storage {
            Storage::Ordinary(values) => values
                .try_reserve(required.saturating_sub(values.len()))
                .map_err(MemoryError::from)?,
            Storage::Admitted(values) => {
                values.reserve(required.saturating_sub(values.len()))?;
            }
        }
        Ok(())
    }
    pub(crate) fn push(&mut self, task: T) -> Result<()> {
        Ok(self.try_push(task)?)
    }
    pub(crate) fn try_push(&mut self, task: T) -> std::result::Result<(), MemoryError> {
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
