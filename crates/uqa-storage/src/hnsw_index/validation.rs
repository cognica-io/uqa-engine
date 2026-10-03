//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Structural validation streams nodes and spills reachability workspaces.

use super::{
    prepare::{check, Control},
    store::Map,
    types::HNSWIndex,
};
use crate::{StorageBackendError, StorageBackendResult};
use uqa_core::memory::BudgetedVec;

impl HNSWIndex {
    pub fn validate_invariants(&self) -> StorageBackendResult<()> {
        self.validate_controlled(None)
    }

    pub(super) fn validate_controlled(&self, control: Control<'_>) -> StorageBackendResult<()> {
        check(control)?;
        let mut computed_max = 0;
        for entry in self.nodes.iter() {
            check(control)?;
            let (_, node) = entry?;
            computed_max = computed_max.max(node.level);
            if node.id >= self.next_node_id {
                return Err(corrupt("next node id does not exceed persisted node ids"));
            }
        }
        if self.nodes.is_empty() {
            if self.entry_point.is_some() || self.max_level != 0 {
                return Err(corrupt("empty graph has an entry point or non-zero level"));
            }
        } else {
            let id = self
                .entry_point
                .ok_or_else(|| corrupt("non-empty graph has no valid entry point"))?;
            let entry = self
                .node(id)?
                .ok_or_else(|| corrupt("non-empty graph has no valid entry point"))?;
            if self.max_level != computed_max || entry.level != self.max_level {
                return Err(corrupt(&format!(
                    "entry/max level mismatch: entry={}, metadata={}, computed={computed_max}",
                    entry.level, self.max_level
                )));
            }
        }
        self.validate_edges(control)?;
        self.validate_reachability(control)
    }

    fn validate_edges(&self, control: Control<'_>) -> StorageBackendResult<()> {
        for entry in self.nodes.iter() {
            check(control)?;
            let (_, node) = entry?;
            if node.neighbors.len() != node.level + 1 {
                return Err(corrupt(&format!(
                    "node {} adjacency layer count does not match its level",
                    node.id
                )));
            }
            for (layer, neighbors) in node.neighbors.iter().enumerate() {
                let mut unique = BudgetedVec::new(&self.memory);
                unique.extend_from_slice(neighbors)?;
                unique.sort_unstable();
                if unique.windows(2).any(|pair| pair[0] == pair[1])
                    || unique.binary_search(&node.id).is_ok()
                {
                    return Err(corrupt(&format!(
                        "node {} layer {layer} contains duplicate or self edges",
                        node.id
                    )));
                }
                if neighbors.len() > self.max_connections(layer) {
                    return Err(corrupt(&format!(
                        "node {} layer {layer} exceeds the degree bound",
                        node.id
                    )));
                }
                for neighbor_id in neighbors {
                    check(control)?;
                    let neighbor = self.node(*neighbor_id)?.ok_or_else(|| {
                        corrupt(&format!(
                            "node {} layer {layer} references missing node {neighbor_id}",
                            node.id
                        ))
                    })?;
                    if neighbor.level < layer || !neighbor.neighbors[layer].contains(&node.id) {
                        return Err(corrupt(&format!(
                            "edge {} <-> {neighbor_id} is invalid at layer {layer}",
                            node.id
                        )));
                    }
                }
            }
        }
        Ok(())
    }

    fn validate_reachability(&self, control: Control<'_>) -> StorageBackendResult<()> {
        let Some(entry_point) = self.entry_point else {
            return Ok(());
        };
        let mut reachable = Map::new(&self.memory, self.memory.limit() / 32);
        let mut pending = Map::new(&self.memory, self.memory.limit() / 32);
        reachable.insert(u128::from(entry_point), 0_u64, control)?;
        pending.insert(0, entry_point, control)?;
        let mut sequence = 1_u128;
        loop {
            check(control)?;
            let next = pending.next(None)?.map(|(key, value)| (key, *value));
            let Some((key, node_id)) = next else {
                break;
            };
            pending.remove(key, control)?;
            let node = self
                .node(node_id)?
                .ok_or_else(|| corrupt("reachability references missing node"))?;
            for neighbor in &node.neighbors[0] {
                check(control)?;
                if reachable.get(u128::from(*neighbor))?.is_none() {
                    reachable.insert(u128::from(*neighbor), 0, control)?;
                    pending.insert(sequence, *neighbor, control)?;
                    sequence += 1;
                }
            }
        }
        if reachable.len() != self.nodes.len() {
            return Err(corrupt(&format!(
                "layer-zero graph reaches {} of {} nodes from entry point {entry_point}",
                reachable.len(),
                self.nodes.len()
            )));
        }
        Ok(())
    }
}

fn corrupt(message: &str) -> StorageBackendError {
    StorageBackendError::Other(format!("corrupt HNSW graph: {message}"))
}
