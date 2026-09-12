//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Incremental minimization of sorted UTF-16 dictionary surfaces.

use std::collections::HashMap;

use super::{Arc, Lexicon, Node};
use crate::nori::error::invalid;
use crate::nori::DictionaryResult;

#[derive(Default, Hash, PartialEq, Eq, Clone)]
struct State {
    terminal: bool,
    arcs: Vec<(u16, u32)>,
}

pub(crate) struct Builder {
    previous: Option<Vec<u16>>,
    pending: Vec<State>,
    registry: HashMap<State, u32>,
    nodes: Vec<Node>,
    arcs: Vec<Arc>,
}

impl Builder {
    pub fn new() -> Self {
        Self {
            previous: None,
            pending: vec![State::default()],
            registry: HashMap::new(),
            nodes: Vec::new(),
            arcs: Vec::new(),
        }
    }

    pub fn insert(&mut self, surface: Vec<u16>) -> DictionaryResult<()> {
        let common = if let Some(previous) = &self.previous {
            if previous >= &surface {
                return Err(invalid(
                    "surface input",
                    "surfaces are duplicate or not in UTF-16 order",
                ));
            }
            previous
                .iter()
                .zip(&surface)
                .take_while(|(left, right)| left == right)
                .count()
        } else {
            0
        };
        self.minimize(common)?;
        self.pending.try_reserve(surface.len() - common)?;
        for _ in common..surface.len() {
            self.pending.push(State::default());
        }
        self.pending
            .last_mut()
            .expect("builder retains its root")
            .terminal = true;
        self.previous = Some(surface);
        Ok(())
    }

    fn minimize(&mut self, prefix: usize) -> DictionaryResult<()> {
        while self.pending.len() > prefix + 1 {
            let index = self.pending.len() - 1;
            let state = self.pending.pop().expect("pending path contains a child");
            let target = self.intern(state)?;
            let label = self.previous.as_ref().expect("pending suffix has an input")[index - 1];
            let parent = self.pending.last_mut().expect("builder retains its root");
            parent.arcs.try_reserve(1)?;
            parent.arcs.push((label, target));
        }
        Ok(())
    }

    fn intern(&mut self, state: State) -> DictionaryResult<u32> {
        if let Some(index) = self.registry.get(&state) {
            return Ok(*index);
        }
        let index = u32::try_from(self.nodes.len())
            .map_err(|_| invalid("lexicon encoder", "too many states"))?;
        let first_arc = u32::try_from(self.arcs.len())
            .map_err(|_| invalid("lexicon encoder", "too many arcs"))?;
        let arc_count = u32::try_from(state.arcs.len())
            .map_err(|_| invalid("lexicon encoder", "too many arcs"))?;
        self.arcs.try_reserve(state.arcs.len())?;
        let mut suffixes = u32::from(state.terminal);
        for &(label, target) in &state.arcs {
            self.arcs.push(Arc {
                label,
                target,
                output: suffixes,
            });
            suffixes = suffixes
                .checked_add(self.nodes[target as usize].suffixes)
                .ok_or_else(|| invalid("lexicon encoder", "too many accepted words"))?;
        }
        self.nodes.try_reserve(1)?;
        self.nodes.push(Node {
            first_arc,
            arc_count,
            suffixes,
            terminal: state.terminal,
        });
        self.registry.try_reserve(1)?;
        self.registry.insert(state, index);
        Ok(index)
    }

    pub fn finish(mut self, maximum_word_length: usize) -> DictionaryResult<Lexicon> {
        self.minimize(0)?;
        let state = self.pending.pop().expect("builder retains its root");
        let root = self.intern(state)?;
        let lexicon = Lexicon {
            nodes: self.nodes,
            arcs: self.arcs,
            root,
        };
        lexicon.validate(maximum_word_length)?;
        Ok(lexicon)
    }
}
