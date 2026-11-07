//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::Children;
use crate::memory::{BudgetedVec, MemoryBudget, MemoryError};

pub(super) enum Stack<'a> {
    Ordinary(Vec<Children<'a>>),
    Admitted(BudgetedVec<Children<'a>>),
}

impl<'a> Stack<'a> {
    pub(super) fn new(budget: Option<&MemoryBudget>) -> Self {
        budget.map_or_else(
            || Self::Ordinary(Vec::new()),
            |budget| Self::Admitted(BudgetedVec::new(budget)),
        )
    }

    pub(super) fn push(&mut self, value: Children<'a>) -> Result<(), MemoryError> {
        match self {
            Self::Ordinary(values) => {
                values.try_reserve(1)?;
                values.push(value);
                Ok(())
            }
            Self::Admitted(values) => values.push(value),
        }
    }

    pub(super) fn last_mut(&mut self) -> Option<&mut Children<'a>> {
        match self {
            Self::Ordinary(values) => values.last_mut(),
            Self::Admitted(values) => values.last_mut(),
        }
    }

    pub(super) fn pop(&mut self) -> Option<Children<'a>> {
        match self {
            Self::Ordinary(values) => values.pop(),
            Self::Admitted(values) => values.pop(),
        }
    }
}
