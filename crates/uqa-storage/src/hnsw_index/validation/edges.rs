//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{corrupt, HNSWIndex, NodeRead, StorageBackendResult, StorageReadControl};
use crate::spill_map::Scratch;

enum Stage {
    Node,
    Layer,
    Unique,
    Degree,
    Neighbor,
    Reciprocal,
}

pub(super) struct Edges<'a> {
    index: &'a HNSWIndex,
    after: Option<u128>,
    node: Option<NodeRead<'a>>,
    neighbor: Option<NodeRead<'a>>,
    unique: Option<Scratch>,
    stage: Stage,
    layer: usize,
    offset: usize,
    reciprocal: usize,
}

impl<'a> Edges<'a> {
    pub(super) fn new(index: &'a HNSWIndex) -> Self {
        Self {
            index,
            after: None,
            node: None,
            neighbor: None,
            unique: None,
            stage: Stage::Node,
            layer: 0,
            offset: 0,
            reciprocal: 0,
        }
    }

    /// One transition visits a metadata boundary, one adjacency identity, or at most 512 reverse identities. Each private-set lookup/update has at most 128 tree steps and performs no corpus maintenance.
    pub(super) fn advance(&mut self, control: &StorageReadControl) -> StorageBackendResult<bool> {
        match self.stage {
            Stage::Node => {
                let Some(key) = self.index.nodes.next_key(self.after, control.memory())? else {
                    return Ok(true);
                };
                let node = NodeRead::open(&self.index.nodes, key, control)?
                    .ok_or_else(|| corrupt("edge owner disappeared"))?;
                if node.level.checked_add(1) != Some(node.layer_count()) {
                    return Err(corrupt(&format!(
                        "node {} adjacency layer count does not match its level",
                        node.id
                    )));
                }
                self.after = Some(key);
                self.node = Some(node);
                self.layer = 0;
                self.stage = Stage::Layer;
            }
            Stage::Layer => {
                let node = self.node.as_ref().expect("selected edge owner");
                if self.layer == node.layer_count() {
                    self.node = None;
                    self.stage = Stage::Node;
                } else {
                    if let Some(unique) = &mut self.unique {
                        unique.clear();
                    }
                    self.offset = 0;
                    self.stage = Stage::Unique;
                }
            }
            Stage::Unique => self.unique(control)?,
            Stage::Degree => {
                let node = self.node.as_ref().expect("selected edge owner");
                if node.neighbor_count(self.layer)? > self.index.max_connections(self.layer) {
                    return Err(corrupt(&format!(
                        "node {} layer {} exceeds the degree bound",
                        node.id, self.layer
                    )));
                }
                self.offset = 0;
                self.stage = Stage::Neighbor;
            }
            Stage::Neighbor => self.neighbor(control)?,
            Stage::Reciprocal => self.reciprocal(control)?,
        }
        Ok(false)
    }

    fn unique(&mut self, control: &StorageReadControl) -> StorageBackendResult<()> {
        let node = self.node.as_ref().expect("selected edge owner");
        if self.offset == node.neighbor_count(self.layer)? {
            self.stage = Stage::Degree;
            return Ok(());
        }
        let mut identity = [0];
        node.read_neighbors(self.layer, self.offset, &mut identity, control)?;
        if self.unique.is_none() {
            self.unique = Some(Scratch::new(control)?);
        }
        let unique = self.unique.as_mut().expect("selected layer owns set");
        if identity[0] == node.id || unique.get(u128::from(identity[0]))?.is_some() {
            return Err(corrupt(&format!(
                "node {} layer {} contains duplicate or self edges",
                node.id, self.layer
            )));
        }
        unique.insert(u128::from(identity[0]), 0)?;
        self.offset += 1;
        Ok(())
    }

    fn neighbor(&mut self, control: &StorageReadControl) -> StorageBackendResult<()> {
        let node = self.node.as_ref().expect("selected edge owner");
        if self.offset == node.neighbor_count(self.layer)? {
            self.layer += 1;
            self.stage = Stage::Layer;
            return Ok(());
        }
        let mut identity = [0];
        node.read_neighbors(self.layer, self.offset, &mut identity, control)?;
        let neighbor = NodeRead::open(&self.index.nodes, u128::from(identity[0]), control)?
            .ok_or_else(|| {
                corrupt(&format!(
                    "node {} layer {} references missing node {}",
                    node.id, self.layer, identity[0]
                ))
            })?;
        if neighbor.level < self.layer {
            return Err(self.invalid_edge(neighbor.id));
        }
        self.neighbor = Some(neighbor);
        self.reciprocal = 0;
        self.stage = Stage::Reciprocal;
        Ok(())
    }

    fn reciprocal(&mut self, control: &StorageReadControl) -> StorageBackendResult<()> {
        let node = self.node.as_ref().expect("selected edge owner");
        let neighbor = self.neighbor.as_ref().expect("selected reverse edge owner");
        let mut values = [0; 512];
        let count = neighbor.read_neighbors(self.layer, self.reciprocal, &mut values, control)?;
        if values[..count].contains(&node.id) {
            self.neighbor = None;
            self.offset += 1;
            self.stage = Stage::Neighbor;
        } else {
            self.reciprocal += count;
            if self.reciprocal == neighbor.neighbor_count(self.layer)? {
                return Err(self.invalid_edge(neighbor.id));
            }
        }
        Ok(())
    }

    fn invalid_edge(&self, neighbor: u64) -> crate::StorageBackendError {
        corrupt(&format!(
            "edge {} <-> {neighbor} is invalid at layer {}",
            self.node.as_ref().expect("selected edge owner").id,
            self.layer
        ))
    }
}
