//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered NFA frontiers retain visited states and their capture slots in the caller allowance.

use regex_automata::{nfa::thompson::NFA, util::primitives::StateID};
use uqa_core::memory::{BudgetedVec, MemoryBudget, MemoryError};

use super::{control::Control, ABSENT};
use crate::AnalysisResult;

pub(super) struct States {
    pub(super) order: BudgetedVec<StateID>,
    positions: BudgetedVec<usize>,
    slots: BudgetedVec<usize>,
    width: usize,
}

impl States {
    pub(super) fn new(
        nfa: &NFA,
        width: usize,
        budget: &MemoryBudget,
        control: &mut Control<'_>,
    ) -> AnalysisResult<Self> {
        let count = nfa.states().len();
        let slot_count = count.checked_mul(width).ok_or(MemoryError::SizeOverflow)?;
        let mut order = BudgetedVec::new(budget);
        order.reserve(count)?;
        Ok(Self {
            order,
            positions: control.buffer(budget, count, 0)?,
            slots: control.buffer(budget, slot_count, ABSENT)?,
            width,
        })
    }

    pub(super) fn insert(&mut self, state: StateID) -> AnalysisResult<bool> {
        let position = &mut self.positions[state.as_usize()];
        if self.order.get(*position) == Some(&state) {
            return Ok(false);
        }
        *position = self.order.len();
        self.order.push(state)?;
        Ok(true)
    }

    pub(super) fn clear(&mut self) {
        self.order.clear();
    }

    pub(super) fn slots(&mut self, state: StateID) -> &mut [usize] {
        let start = state.as_usize() * self.width;
        &mut self.slots[start..start + self.width]
    }
}
