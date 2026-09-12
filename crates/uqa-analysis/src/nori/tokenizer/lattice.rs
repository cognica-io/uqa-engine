//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bounded rolling positions, retaining candidate insertion order for exact cost ties.

use std::collections::VecDeque;

use super::NoriLimits;
use crate::nori::error::{check_limit, invalid};
use crate::nori::DictionaryResult;

#[derive(Debug, Clone, Copy)]
pub(super) enum WordId {
    Known(u32),
    Unknown(u32),
    User(u32),
}

#[derive(Debug, Clone, Copy)]
pub(super) struct Node {
    pub cost: i32,
    pub right: u16,
    pub back_pos: usize,
    pub word_pos: usize,
    pub back_index: usize,
    pub word: WordId,
}

pub(super) struct Lattice {
    base: usize,
    positions: VecDeque<Vec<Node>>,
    candidates: usize,
    limits: NoriLimits,
}

impl Lattice {
    pub fn new(limits: NoriLimits) -> DictionaryResult<Self> {
        let mut lattice = Self {
            base: 0,
            positions: VecDeque::new(),
            candidates: 0,
            limits,
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
        )?;
        Ok(lattice)
    }

    pub fn next_pos(&self) -> usize {
        self.base + self.positions.len()
    }

    pub fn ensure(&mut self, position: usize) -> DictionaryResult<()> {
        let index = position
            .checked_sub(self.base)
            .ok_or_else(|| invalid("Nori lattice", "position was already released"))?;
        let required = index
            .checked_add(1)
            .ok_or_else(|| invalid("Nori lattice", "position overflow"))?;
        check_limit(
            "Nori lattice positions",
            required,
            self.limits.max_lattice_positions,
        )?;
        if required > self.positions.len() {
            self.positions
                .try_reserve(required - self.positions.len())?;
            self.positions.resize_with(required, Vec::new);
        }
        Ok(())
    }

    pub fn get(&self, position: usize) -> &[Node] {
        &self.positions[position - self.base]
    }

    pub fn push(&mut self, position: usize, node: Node) -> DictionaryResult<()> {
        self.ensure(position)?;
        check_limit(
            "Nori lattice candidates",
            self.candidates + 1,
            self.limits.max_lattice_candidates,
        )?;
        let nodes = &mut self.positions[position - self.base];
        nodes.try_reserve(1)?;
        nodes.push(node);
        self.candidates += 1;
        Ok(())
    }

    pub fn rebase(&mut self, position: usize) {
        self.positions[position - self.base][0].cost = 0;
    }

    pub fn prune(&mut self, from: usize, keep: usize, index: usize) {
        let node = self.get(keep)[index];
        for position in from..self.next_pos() {
            let nodes = &mut self.positions[position - self.base];
            self.candidates -= nodes.len();
            nodes.clear();
            if position == keep {
                nodes.push(node);
                self.candidates += 1;
            }
        }
    }

    pub fn release_before(&mut self, position: usize) {
        while self.base < position {
            self.candidates -= self.positions.pop_front().expect("live prefix").len();
            self.base += 1;
        }
    }
}
