//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered forward traversal, frontier commits, and bounded forced backtraces.

use uqa_core::memory::MemoryBudget;

use super::lattice::{Lattice, LatticeConfig};
use crate::AnalysisResult;

pub(crate) struct Traversal<'a, C> {
    pub lattice: Lattice<C>,
    pub position: usize,
    pub last_backtrace: usize,
    input_len: usize,
    work: usize,
    pub poll: &'a mut dyn FnMut() -> AnalysisResult<()>,
}

impl<'a, C: LatticeConfig> Traversal<'a, C> {
    pub fn new(
        input_len: usize,
        limits: C,
        budget: &MemoryBudget,
        poll: &'a mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<Self> {
        Ok(Self {
            lattice: Lattice::new(limits, budget, poll)?,
            position: 0,
            last_backtrace: 0,
            input_len,
            work: 0,
            poll,
        })
    }

    pub fn tick(&mut self) -> AnalysisResult<()> {
        self.work = (self.work + 1) % 1024;
        if self.work == 0 {
            (self.poll)()?;
        }
        Ok(())
    }

    fn ensure_position(&mut self) -> AnalysisResult<()> {
        self.lattice.ensure(self.position, self.poll)
    }
}

/// Language-owned matching and emission surround the same ordered single-path search.
pub(crate) trait Search<'a> {
    type Config: LatticeConfig;
    type Batch: Default;

    fn traversal(&self) -> &Traversal<'a, Self::Config>;
    fn traversal_mut(&mut self) -> &mut Traversal<'a, Self::Config>;
    fn has_pending(&self) -> bool;
    fn extend(&mut self, batch: &mut Self::Batch) -> AnalysisResult<()>;
    fn backtrace(&mut self, position: usize, index: usize) -> AnalysisResult<()>;
    fn eos_cost(&self, right: u16) -> i32;
}

pub(crate) fn forward<'a, S: Search<'a>>(state: &mut S) -> AnalysisResult<bool> {
    // The reference resets language-local overlap bounds for each consumed pending batch.
    let mut batch = S::Batch::default();
    while state.traversal().position < state.traversal().input_len {
        state.traversal_mut().tick()?;
        state.traversal_mut().ensure_position()?;
        let position = state.traversal().position;
        let traversal = state.traversal();
        if traversal.lattice.get(position).is_empty() {
            state.traversal_mut().position += 1;
            continue;
        }
        let frontier = traversal.lattice.next_pos() == position + 1;
        if position > traversal.last_backtrace
            && frontier
            && traversal.lattice.get(position).len() == 1
        {
            state.backtrace(position, 0)?;
            state.traversal_mut().lattice.rebase(position);
            if state.has_pending() {
                return Ok(false);
            }
        }
        if position - state.traversal().last_backtrace >= 1024 {
            force_backtrace(state)?;
            if state.has_pending() {
                return Ok(false);
            }
            continue;
        }
        state.extend(&mut batch)?;
        state.traversal_mut().position += 1;
    }
    finish(state)?;
    Ok(true)
}

fn finish<'a, S: Search<'a>>(state: &mut S) -> AnalysisResult<()> {
    let position = state.traversal().position;
    if position > 0 {
        state.traversal_mut().ensure_position()?;
        let mut best = None;
        let mut least_cost = i32::MAX;
        for index in 0..state.traversal().lattice.get(position).len() {
            state.traversal_mut().tick()?;
            let node = state.traversal().lattice.get(position)[index];
            let cost = node.cost.wrapping_add(state.eos_cost(node.right));
            if cost < least_cost {
                least_cost = cost;
                best = Some(index);
            }
        }
        let best = best.ok_or_else(|| state.traversal().lattice.invalid("no complete path"))?;
        state.backtrace(position, best)?;
    }
    Ok(())
}

fn force_backtrace<'a, S: Search<'a>>(state: &mut S) -> AnalysisResult<()> {
    let mut best = None;
    let mut least = i32::MAX;
    let traversal = state.traversal();
    for position in traversal.position..traversal.lattice.next_pos() {
        state.traversal_mut().tick()?;
        for index in 0..state.traversal().lattice.get(position).len() {
            state.traversal_mut().tick()?;
            let node = state.traversal().lattice.get(position)[index];
            if node.cost < least {
                least = node.cost;
                best = Some((position, index));
            }
        }
    }
    let (position, index) = best.ok_or_else(|| {
        state
            .traversal()
            .lattice
            .invalid("no live path at forced backtrace")
    })?;
    let traversal = state.traversal_mut();
    traversal
        .lattice
        .prune(traversal.position, position, index, traversal.poll)?;
    state.backtrace(position, 0)?;
    let traversal = state.traversal_mut();
    traversal.lattice.rebase(position);
    traversal.position = position;
    Ok(())
}
