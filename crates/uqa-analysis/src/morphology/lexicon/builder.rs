//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Incremental minimization of sorted UTF-16 dictionary surfaces.

use std::{
    collections::HashMap,
    hash::{BuildHasher, Hasher},
};

use super::{Arc, Lexicon, Node};
use crate::morphology::error::invalid;
use crate::morphology::{work::Work, DictionaryResult};

#[derive(Default)]
struct State {
    terminal: bool,
    arcs: Vec<(u16, u32)>,
}

pub(crate) struct Builder {
    previous: Option<Vec<u16>>,
    pending: Vec<State>,
    registry: HashMap<u64, u32>,
    registry_next: Vec<Option<u32>>,
    nodes: Vec<Node>,
    arcs: Vec<Arc>,
}

impl Builder {
    pub fn new() -> Self {
        Self {
            previous: None,
            pending: vec![State::default()],
            registry: HashMap::new(),
            registry_next: Vec::new(),
            nodes: Vec::new(),
            arcs: Vec::new(),
        }
    }

    #[cfg(any(test, feature = "nori-tools", feature = "kuromoji-tools"))]
    pub fn insert(self, surface: Vec<u16>) -> DictionaryResult<Self> {
        crate::morphology::work::inline(self.insert_with(surface, &mut ()))
    }

    pub async fn insert_with(
        mut self,
        surface: Vec<u16>,
        work: &mut impl Work,
    ) -> DictionaryResult<Self> {
        let common = if let Some(previous) = &self.previous {
            let (common, order) =
                super::common_prefix_order(previous.iter().copied(), surface.iter().copied(), work)
                    .await?;
            if !order.is_lt() {
                return Err(unordered());
            }
            common
        } else {
            0
        };
        self.minimize(common, work).await?;
        self.pending.try_reserve(surface.len() - common)?;
        for index in common..surface.len() {
            work.records(index - common).await?;
            self.pending.push(State::default());
        }
        self.pending
            .last_mut()
            .expect("builder retains its root")
            .terminal = true;
        self.previous = Some(surface);
        Ok(self)
    }

    async fn minimize(&mut self, prefix: usize, work: &mut impl Work) -> DictionaryResult<()> {
        while self.pending.len() > prefix + 1 {
            work.checkpoint().await?;
            let index = self.pending.len() - 1;
            let state = self.pending.pop().expect("pending path contains a child");
            let target = self.intern(state, work).await?;
            let label = self.previous.as_ref().expect("pending suffix has an input")[index - 1];
            let parent = self.pending.last_mut().expect("builder retains its root");
            parent.arcs.try_reserve(1)?;
            parent.arcs.push((label, target));
        }
        Ok(())
    }

    async fn intern(&mut self, state: State, work: &mut impl Work) -> DictionaryResult<u32> {
        let mut hash = self.registry.hasher().build_hasher();
        hash.write_u8(u8::from(state.terminal));
        hash.write_usize(state.arcs.len());
        for (index, &(label, target)) in state.arcs.iter().enumerate() {
            work.records(index).await?;
            hash.write_u16(label);
            hash.write_u32(target);
        }
        self.intern_hashed(state, hash.finish(), work).await
    }

    async fn intern_hashed(
        &mut self,
        state: State,
        hash: u64,
        work: &mut impl Work,
    ) -> DictionaryResult<u32> {
        let head = self.registry.get(&hash).copied();
        let mut candidate = head;
        while let Some(index) = candidate {
            work.checkpoint().await?;
            let node = &self.nodes[index as usize];
            if node.terminal == state.terminal && node.arc_count as usize == state.arcs.len() {
                let start = node.first_arc as usize;
                let mut equal = true;
                for (offset, &(label, target)) in state.arcs.iter().enumerate() {
                    work.records(offset).await?;
                    let arc = &self.arcs[start + offset];
                    if arc.label != label || arc.target != target {
                        equal = false;
                        break;
                    }
                }
                if equal {
                    return Ok(index);
                }
            }
            candidate = self.registry_next[index as usize];
        }
        work.checkpoint().await?;
        let index = u32::try_from(self.nodes.len())
            .map_err(|_| invalid("lexicon encoder", "too many states"))?;
        let first_arc = u32::try_from(self.arcs.len())
            .map_err(|_| invalid("lexicon encoder", "too many arcs"))?;
        let arc_count = u32::try_from(state.arcs.len())
            .map_err(|_| invalid("lexicon encoder", "too many arcs"))?;
        self.arcs.try_reserve(state.arcs.len())?;
        let mut suffixes = u32::from(state.terminal);
        for (offset, &(label, target)) in state.arcs.iter().enumerate() {
            work.records(offset).await?;
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
        self.registry_next.try_reserve(1)?;
        self.registry_next.push(head);
        self.registry.try_reserve(1)?;
        self.registry.insert(hash, index);
        Ok(index)
    }

    #[cfg(any(test, feature = "nori-tools", feature = "kuromoji-tools"))]
    pub fn finish(self, maximum_word_length: usize) -> DictionaryResult<Lexicon> {
        crate::morphology::work::inline(self.finish_with(maximum_word_length, &mut ()))
    }

    pub async fn finish_with(
        mut self,
        maximum_word_length: usize,
        work: &mut impl Work,
    ) -> DictionaryResult<Lexicon> {
        self.minimize(0, work).await?;
        let state = self.pending.pop().expect("builder retains its root");
        let root = self.intern(state, work).await?;
        let lexicon = Lexicon {
            nodes: self.nodes,
            arcs: self.arcs,
            root,
        };
        lexicon.validate_with(maximum_word_length, work).await?;
        Ok(lexicon)
    }
}

fn unordered() -> crate::morphology::DictionaryError {
    invalid(
        "surface input",
        "surfaces are duplicate or not in UTF-16 order",
    )
}

#[cfg(test)]
mod tests;
