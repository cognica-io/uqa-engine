//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Structural and reachability invariants for HNSW graphs.

use std::collections::BTreeSet;

use super::types::HNSWIndex;
use crate::{StorageBackendError, StorageBackendResult};

impl HNSWIndex {
    pub fn validate_invariants(&self) -> StorageBackendResult<()> {
        let computed_max = self
            .nodes
            .values()
            .map(|node| node.level)
            .max()
            .unwrap_or(0);
        if self.nodes.is_empty() {
            if self.entry_point.is_some() || self.max_level != 0 {
                return Err(corrupt("empty graph has an entry point or non-zero level"));
            }
        } else {
            let entry = self
                .entry_point
                .and_then(|id| self.nodes.get(&id))
                .ok_or_else(|| corrupt("non-empty graph has no valid entry point"))?;
            if self.max_level != computed_max || entry.level != self.max_level {
                return Err(corrupt(&format!(
                    "entry/max level mismatch: entry={}, metadata={}, computed={computed_max}",
                    entry.level, self.max_level
                )));
            }
        }
        if self
            .nodes
            .keys()
            .next_back()
            .is_some_and(|max_node_id| self.next_node_id <= *max_node_id)
        {
            return Err(corrupt("next node id does not exceed persisted node ids"));
        }
        self.validate_edges()?;
        self.validate_reachability()
    }

    fn validate_edges(&self) -> StorageBackendResult<()> {
        for node in self.nodes.values() {
            if node.neighbors.len() != node.level + 1 {
                return Err(corrupt(&format!(
                    "node {} adjacency layer count does not match its level",
                    node.id
                )));
            }
            for (layer, neighbors) in node.neighbors.iter().enumerate() {
                let unique = neighbors.iter().copied().collect::<BTreeSet<_>>();
                if unique.len() != neighbors.len() || unique.contains(&node.id) {
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
                    let neighbor = self.nodes.get(neighbor_id).ok_or_else(|| {
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

    fn validate_reachability(&self) -> StorageBackendResult<()> {
        let Some(entry_point) = self.entry_point else {
            return Ok(());
        };
        let mut reachable = BTreeSet::from([entry_point]);
        let mut pending = vec![entry_point];
        while let Some(node_id) = pending.pop() {
            let node = self
                .nodes
                .get(&node_id)
                .expect("edge references were validated before reachability");
            for neighbor in &node.neighbors[0] {
                if reachable.insert(*neighbor) {
                    pending.push(*neighbor);
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
