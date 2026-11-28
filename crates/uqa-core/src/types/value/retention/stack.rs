//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::Children;
use crate::memory::{BudgetedVec, MemoryBudget, MemoryError};
use smallvec::{Array, SmallVec};

pub(super) struct Stack<'a, A: Array<Item = Children<'a>>> {
    inline: SmallVec<A>,
    overflow: Overflow<'a>,
}

enum Overflow<'a> {
    Ordinary(Vec<Children<'a>>),
    Admitted(BudgetedVec<Children<'a>>),
}

impl<'a, A: Array<Item = Children<'a>>> Stack<'a, A> {
    pub(super) fn new(budget: Option<&MemoryBudget>) -> Self {
        Self {
            inline: SmallVec::new(),
            overflow: budget.map_or_else(
                || Overflow::Ordinary(Vec::new()),
                |budget| Overflow::Admitted(BudgetedVec::new(budget)),
            ),
        }
    }

    pub(super) fn push(&mut self, value: Children<'a>) -> Result<(), MemoryError> {
        if self.inline.len() < A::size() {
            self.inline.push(value);
            return Ok(());
        }
        match &mut self.overflow {
            Overflow::Ordinary(values) => {
                values.try_reserve(1)?;
                values.push(value);
                Ok(())
            }
            Overflow::Admitted(values) => values.push(value),
        }
    }

    pub(super) fn last_mut(&mut self) -> Option<&mut Children<'a>> {
        match &mut self.overflow {
            Overflow::Ordinary(values) => values.last_mut(),
            Overflow::Admitted(values) => values.last_mut(),
        }
        .or_else(|| self.inline.last_mut())
    }

    pub(super) fn pop(&mut self) -> Option<Children<'a>> {
        match &mut self.overflow {
            Overflow::Ordinary(values) => values.pop(),
            Overflow::Admitted(values) => values.pop(),
        }
        .or_else(|| self.inline.pop())
    }
}
