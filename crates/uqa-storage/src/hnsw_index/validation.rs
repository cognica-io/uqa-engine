//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! One resumable structural validator preserves identity, edge and reachability diagnostic order.

use super::{prepare::Control, store::NodeRead, types::HNSWIndex};
use crate::{
    image_io::ImagePreparation, read_control::StorageReadControl, StorageBackendError,
    StorageBackendResult,
};

mod edges;
mod reachability;
#[cfg(test)]
pub(super) mod tests;

enum Stage {
    Counts,
    Identities,
    Entry,
    Edges,
    Reachability,
    Complete,
}

pub(super) struct Validation<'a> {
    index: &'a HNSWIndex,
    stage: Stage,
    edges: Option<edges::Edges<'a>>,
    reachability: Option<reachability::Reachability<'a>>,
    after: Option<u128>,
    computed_max: usize,
}

impl HNSWIndex {
    pub fn validate_invariants(&self) -> StorageBackendResult<()> {
        self.validate_controlled(None)
    }

    pub(super) fn validate_controlled(&self, control: Control<'_>) -> StorageBackendResult<()> {
        let fallback;
        let control = if let Some(control) = control {
            control
        } else {
            fallback = StorageReadControl::new(&self.memory, &uqa_core::CancellationToken::new());
            &fallback
        };
        let mut validation = Validation::new(self);
        while !validation.advance(128, control)?.ready {}
        Ok(())
    }
}

impl<'a> Validation<'a> {
    pub(super) fn new(index: &'a HNSWIndex) -> Self {
        Self {
            index,
            stage: Stage::Counts,
            edges: None,
            reachability: None,
            after: None,
            computed_max: 0,
        }
    }

    pub(super) fn advance(
        &mut self,
        maximum: usize,
        control: &StorageReadControl,
    ) -> StorageBackendResult<ImagePreparation> {
        control.check()?;
        let mut positions = 0;
        while positions < maximum && !matches!(self.stage, Stage::Complete) {
            control.check()?;
            self.step(control)?;
            positions += 1;
        }
        control.check()?;
        Ok(ImagePreparation {
            positions,
            ready: matches!(self.stage, Stage::Complete),
        })
    }

    fn step(&mut self, control: &StorageReadControl) -> StorageBackendResult<()> {
        match &mut self.stage {
            Stage::Counts => {
                if self.index.raw_vectors.len() != self.index.nodes.len()
                    || self.index.normalized_vectors.len() != self.index.nodes.len()
                {
                    return Err(corrupt("graph and vector identity counts differ"));
                }
                self.stage = Stage::Identities;
            }
            Stage::Identities => {
                if let Some(key) = self.index.nodes.next_key(self.after, control.memory())? {
                    let node = NodeRead::open(&self.index.nodes, key, control)?
                        .ok_or_else(|| corrupt("graph identity disappeared"))?;
                    if key != u128::from(node.id)
                        || !self.index.raw_vectors.contains_key(key)?
                        || !self.index.normalized_vectors.contains_key(key)?
                    {
                        return Err(corrupt("graph and vector identities differ"));
                    }
                    self.computed_max = self.computed_max.max(node.level);
                    if node.id >= self.index.next_node_id {
                        return Err(corrupt("next node id does not exceed persisted node ids"));
                    }
                    self.after = Some(key);
                } else {
                    self.stage = Stage::Entry;
                }
            }
            Stage::Entry => {
                self.entry(control)?;
                self.edges = Some(edges::Edges::new(self.index));
                self.stage = Stage::Edges;
            }
            Stage::Edges => {
                if self
                    .edges
                    .as_mut()
                    .expect("active edge validation")
                    .advance(control)?
                {
                    self.edges = None;
                    self.reachability = Some(reachability::Reachability::new(self.index));
                    self.stage = Stage::Reachability;
                }
            }
            Stage::Reachability => {
                if self
                    .reachability
                    .as_mut()
                    .expect("active reachability validation")
                    .advance(control)?
                {
                    self.reachability = None;
                    self.stage = Stage::Complete;
                }
            }
            Stage::Complete => (),
        }
        Ok(())
    }

    fn entry(&self, control: &StorageReadControl) -> StorageBackendResult<()> {
        if self.index.nodes.is_empty() {
            if self.index.entry_point.is_some() || self.index.max_level != 0 {
                return Err(corrupt("empty graph has an entry point or non-zero level"));
            }
        } else {
            let missing = || corrupt("non-empty graph has no valid entry point");
            let id = self.index.entry_point.ok_or_else(missing)?;
            let entry =
                NodeRead::open(&self.index.nodes, u128::from(id), control)?.ok_or_else(missing)?;
            if self.index.max_level != self.computed_max || entry.level != self.index.max_level {
                return Err(corrupt(&format!(
                    "entry/max level mismatch: entry={}, metadata={}, computed={}",
                    entry.level, self.index.max_level, self.computed_max
                )));
            }
        }
        Ok(())
    }
}

fn corrupt(message: &str) -> StorageBackendError {
    StorageBackendError::Other(format!("corrupt HNSW graph: {message}"))
}
