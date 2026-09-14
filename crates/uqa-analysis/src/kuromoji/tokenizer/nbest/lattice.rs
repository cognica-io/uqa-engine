//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered Japanese alternative paths retain their own bounded node and edge-root storage.

use uqa_core::memory::{BudgetedVec, MemoryBudget};

use super::super::{viterbi::State, word};
use crate::kuromoji::error::{check_limit, invalid};
use crate::morphology::lattice::WordId;
use crate::AnalysisResult;

#[derive(Clone, Copy, Default)]
struct Roots {
    left: Option<usize>,
    right: Option<usize>,
}

#[derive(Clone, Copy)]
struct Node {
    word: Option<WordId>,
    left: Option<usize>,
    right: Option<usize>,
    left_chain: Option<usize>,
    right_chain: Option<usize>,
    left_id: u16,
    right_id: u16,
    word_cost: i32,
    left_cost: i32,
    right_cost: i32,
    next: Option<usize>,
    mark: i8,
}

pub(in super::super) struct Graph {
    roots: BudgetedVec<Roots>,
    nodes: BudgetedVec<Node>,
    base: usize,
    use_eos: bool,
}

impl Graph {
    pub fn new(budget: &MemoryBudget) -> Self {
        Self {
            roots: BudgetedVec::new(budget),
            nodes: BudgetedVec::new(budget),
            base: 0,
            use_eos: false,
        }
    }

    pub fn base(&self) -> usize {
        self.base
    }

    pub fn setup(
        &mut self,
        state: &mut State<'_>,
        end: usize,
        use_eos: bool,
    ) -> AnalysisResult<()> {
        let base = state.traversal.last_backtrace;
        self.base = base;
        self.use_eos = use_eos;
        self.roots.clear();
        self.nodes.clear();
        let size = end - base + 1;
        let mut count = 2_usize;
        for position in base + 1..=end {
            state.n_best_tick()?;
            count = count
                .checked_add(state.traversal.lattice.get(position).len())
                .ok_or_else(|| invalid("Kuromoji N-best", "node count overflow"))?;
        }
        check_limit(
            "Kuromoji N-best nodes",
            count,
            state.limits.max_n_best_nodes,
        )?;
        self.roots.reserve(size)?;
        for _ in 0..size {
            state.n_best_tick()?;
            self.roots.push(Roots::default())?;
        }
        self.nodes.reserve(count)?;
        let word = if base == 0 {
            None
        } else {
            Some(
                state
                    .traversal
                    .lattice
                    .get(base)
                    .first()
                    .ok_or_else(|| invalid("Kuromoji N-best", "missing prefix context"))?
                    .word,
            )
        };
        self.add(state, word, None, Some(0))?;
        self.add(state, None, Some(end - base), None)?;
        for position in (base + 1..=end).rev() {
            state.n_best_tick()?;
            let right = position - base;
            if self.roots[right].left.is_some() {
                for index in 0..state.traversal.lattice.get(position).len() {
                    state.n_best_tick()?;
                    let node = state.traversal.lattice.get(position)[index];
                    self.add(
                        state,
                        Some(node.word),
                        Some(node.back_pos - base),
                        Some(right),
                    )?;
                }
            }
        }
        for position in 1..size - 1 {
            state.n_best_tick()?;
            if self.roots[position].right.is_none() {
                let mut node = self.roots[position].left;
                while let Some(index) = node {
                    state.n_best_tick()?;
                    self.nodes[index].mark = -1;
                    node = self.nodes[index].left_chain;
                }
            }
        }
        self.left_costs(state)?;
        self.right_costs(state)
    }

    fn add(
        &mut self,
        state: &State<'_>,
        word: Option<WordId>,
        left: Option<usize>,
        right: Option<usize>,
    ) -> AnalysisResult<()> {
        let costs = word.map_or(
            word::Costs {
                left: 0,
                right: 0,
                word: 0,
            },
            |word| word::costs(word, state.model, state.user),
        );
        let index = self.nodes.len();
        self.nodes.push(Node {
            word,
            left,
            right,
            left_chain: left.and_then(|position| self.roots[position].left),
            right_chain: right.and_then(|position| self.roots[position].right),
            left_id: costs.left,
            right_id: costs.right,
            word_cost: costs.word,
            left_cost: 0,
            right_cost: 0,
            next: None,
            mark: 0,
        })?;
        if let Some(left) = left {
            self.roots[left].left = Some(index);
        }
        if let Some(right) = right {
            self.roots[right].right = Some(index);
        }
        Ok(())
    }

    fn connection(&self, state: &State<'_>, left: Node, right: Node) -> i32 {
        if right.left_id == 0 && !self.use_eos {
            0
        } else {
            i32::from(
                state
                    .model
                    .connection_cost(left.right_id, right.left_id)
                    .expect("validated N-best contexts"),
            )
        }
    }

    fn left_costs(&mut self, state: &mut State<'_>) -> AnalysisResult<()> {
        for position in 0..self.roots.len() {
            state.n_best_tick()?;
            let mut node_index = self.roots[position].left;
            while let Some(index) = node_index {
                state.n_best_tick()?;
                let node = self.nodes[index];
                node_index = node.left_chain;
                if node.mark < 0 {
                    continue;
                }
                let mut least = i32::MAX;
                let mut best = None;
                let mut previous = self.roots[position].right;
                while let Some(candidate) = previous {
                    state.n_best_tick()?;
                    let left = self.nodes[candidate];
                    previous = left.right_chain;
                    if left.mark < 0 {
                        continue;
                    }
                    let cost = left
                        .left_cost
                        .wrapping_add(left.word_cost)
                        .wrapping_add(self.connection(state, left, node));
                    if cost < least {
                        least = cost;
                        best = Some(candidate);
                    }
                }
                best.ok_or_else(|| invalid("Kuromoji N-best", "no incoming alternative path"))?;
                self.nodes[index].left_cost = least;
            }
        }
        Ok(())
    }

    fn right_costs(&mut self, state: &mut State<'_>) -> AnalysisResult<()> {
        for position in (0..self.roots.len()).rev() {
            state.n_best_tick()?;
            let mut node_index = self.roots[position].right;
            while let Some(index) = node_index {
                state.n_best_tick()?;
                let node = self.nodes[index];
                node_index = node.right_chain;
                if node.mark < 0 {
                    continue;
                }
                let mut least = i32::MAX;
                let mut best = None;
                let mut next = self.roots[position].left;
                while let Some(candidate) = next {
                    state.n_best_tick()?;
                    let right = self.nodes[candidate];
                    next = right.left_chain;
                    if right.mark < 0 {
                        continue;
                    }
                    let cost = right
                        .right_cost
                        .wrapping_add(right.word_cost)
                        .wrapping_add(self.connection(state, node, right));
                    if cost < least {
                        least = cost;
                        best = Some(candidate);
                    }
                }
                self.nodes[index].next =
                    Some(best.ok_or_else(|| {
                        invalid("Kuromoji N-best", "no outgoing alternative path")
                    })?);
                self.nodes[index].right_cost = least;
            }
        }
        Ok(())
    }

    fn mark_span(&mut self, state: &mut State<'_>, reference: usize) -> AnalysisResult<()> {
        let reference = self.nodes[reference];
        let mut next = self.roots[reference.left.expect("real alternative node")].left;
        while let Some(index) = next {
            state.n_best_tick()?;
            if self.nodes[index].right == reference.right {
                self.nodes[index].mark = 1;
            }
            next = self.nodes[index].left_chain;
        }
        Ok(())
    }

    fn cost(&self, index: usize) -> i32 {
        let node = self.nodes[index];
        node.left_cost
            .wrapping_add(node.word_cost)
            .wrapping_add(node.right_cost)
    }

    pub fn register(&mut self, state: &mut State<'_>) -> AnalysisResult<()> {
        let mut next = self.nodes[0].next;
        while let Some(index) = next {
            state.n_best_tick()?;
            if index == 1 {
                break;
            }
            self.mark_span(state, index)?;
            self.emit(state, index)?;
            next = self.nodes[index].next;
        }
        let mut selected = BudgetedVec::new(state.budget);
        loop {
            let mut least = i32::MAX;
            let mut first_span = (None, None);
            selected.clear();
            for index in 2..self.nodes.len() {
                state.n_best_tick()?;
                let node = self.nodes[index];
                if node.mark != 0 {
                    continue;
                }
                let cost = self.cost(index);
                if cost < least {
                    least = cost;
                    first_span = (node.left, node.right);
                    selected.clear();
                    selected.push(index)?;
                } else if cost == least && (node.left, node.right) != first_span {
                    selected.push(index)?;
                }
            }
            if selected.is_empty() {
                break;
            }
            for &index in selected.iter() {
                self.mark_span(state, index)?;
            }
            if self.nodes[1]
                .left_cost
                .wrapping_add(state.options.n_best_cost)
                < self.cost(selected[0])
            {
                break;
            }
            for &index in selected.iter() {
                state.n_best_tick()?;
                self.emit(state, index)?;
            }
        }
        Ok(())
    }

    fn emit(&self, state: &mut State<'_>, index: usize) -> AnalysisResult<()> {
        let node = self.nodes[index];
        super::register_node(
            state,
            node.word.expect("real alternative word"),
            self.base + node.left.expect("real alternative start"),
            self.base + node.right.expect("real alternative end"),
        )
    }

    pub fn probe_delta(
        &self,
        state: &mut State<'_>,
        range: std::ops::Range<usize>,
    ) -> AnalysisResult<i32> {
        let Some(left) = range.start.checked_sub(self.base) else {
            return Ok(i32::MAX);
        };
        let Some(right) = range.end.checked_sub(self.base) else {
            return Ok(i32::MAX);
        };
        if right > self.roots.len() {
            return Ok(i32::MAX);
        }
        let mut next = self
            .roots
            .get(left)
            .ok_or_else(|| {
                invalid(
                    "Kuromoji N-best examples",
                    "probe start exceeds the current fragment",
                )
            })?
            .left;
        let mut cost = i32::MAX;
        while let Some(index) = next {
            state.n_best_tick()?;
            if self.nodes[index].right == Some(right) {
                cost = cost.min(self.cost(index));
            }
            next = self.nodes[index].left_chain;
        }
        Ok(cost.wrapping_sub(self.nodes[1].left_cost))
    }
}
