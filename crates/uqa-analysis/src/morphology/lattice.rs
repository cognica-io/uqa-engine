//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bounded rolling positions, retaining candidate insertion order for exact cost ties.

use uqa_core::memory::{BudgetedDeque, BudgetedVec, MemoryBudget};

use crate::{AnalysisError, AnalysisResult};

/// Language wrappers preserve their public limit diagnostics without changing lattice mechanics.
pub(crate) trait LatticeConfig: Copy {
    fn check_positions(self, required: usize) -> AnalysisResult<()>;
    fn check_candidates(self, required: usize) -> AnalysisResult<()>;
    fn invalid(self, reason: &'static str) -> AnalysisError;
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum WordId {
    Known(u32),
    Unknown(u32),
    User(u32),
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Node {
    pub cost: i32,
    pub right: u16,
    pub back_pos: usize,
    pub word_pos: usize,
    pub back_index: usize,
    pub word: WordId,
}

pub(crate) struct Lattice<C> {
    base: usize,
    positions: BudgetedDeque<BudgetedVec<Node>>,
    candidates: usize,
    limits: C,
    budget: MemoryBudget,
}

impl<C: LatticeConfig> Lattice<C> {
    pub fn invalid(&self, reason: &'static str) -> AnalysisError {
        self.limits.invalid(reason)
    }

    pub fn new(
        limits: C,
        budget: &MemoryBudget,
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<Self> {
        let mut lattice = Self {
            base: 0,
            positions: BudgetedDeque::new(budget),
            candidates: 0,
            limits,
            budget: budget.clone(),
        };
        lattice.push(
            0,
            Node {
                cost: 0,
                right: 0,
                back_pos: 0,
                word_pos: 0,
                back_index: 0,
                word: WordId::Known(0),
            },
            poll,
        )?;
        Ok(lattice)
    }

    pub fn next_pos(&self) -> usize {
        self.base + self.positions.len()
    }

    pub fn ensure(
        &mut self,
        position: usize,
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<()> {
        let index = position
            .checked_sub(self.base)
            .ok_or_else(|| self.limits.invalid("position was already released"))?;
        let required = index
            .checked_add(1)
            .ok_or_else(|| self.limits.invalid("position overflow"))?;
        self.limits.check_positions(required)?;
        if required > self.positions.len() {
            poll()?;
            self.positions.reserve(required - self.positions.len())?;
            while self.positions.len() < required {
                if self.positions.len().is_multiple_of(1024) {
                    poll()?;
                }
                self.positions.push_back(BudgetedVec::new(&self.budget))?;
            }
        }
        Ok(())
    }

    pub fn get(&self, position: usize) -> &[Node] {
        &self.positions[position - self.base]
    }

    pub fn push(
        &mut self,
        position: usize,
        node: Node,
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<()> {
        self.ensure(position, poll)?;
        self.limits.check_candidates(
            self.candidates
                .checked_add(1)
                .ok_or_else(|| self.limits.invalid("candidate count overflow"))?,
        )?;
        let nodes = &mut self.positions[position - self.base];
        nodes.push(node)?;
        self.candidates += 1;
        Ok(())
    }

    pub fn rebase(&mut self, position: usize) {
        self.positions[position - self.base][0].cost = 0;
    }

    #[cfg(feature = "kuromoji")]
    pub fn clear_position(&mut self, position: usize) {
        let nodes = &mut self.positions[position - self.base];
        self.candidates -= nodes.len();
        nodes.clear();
    }

    pub fn prune(
        &mut self,
        from: usize,
        keep: usize,
        index: usize,
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<()> {
        let node = self.get(keep)[index];
        for (work, position) in (from..self.next_pos()).enumerate() {
            if work % 1024 == 0 {
                poll()?;
            }
            let nodes = &mut self.positions[position - self.base];
            self.candidates -= nodes.len();
            if position == keep {
                nodes[0] = node;
                nodes.truncate(1);
                self.candidates += 1;
            } else {
                nodes.clear();
            }
        }
        Ok(())
    }

    pub fn release_before(
        &mut self,
        position: usize,
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<()> {
        let mut work = 0;
        while self.base < position {
            if work % 1024 == 0 {
                poll()?;
            }
            self.candidates -= self.positions.pop_front().expect("live prefix").len();
            self.base += 1;
            work += 1;
        }
        Ok(())
    }
}
