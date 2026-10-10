//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Greedy hierarchy traversal and bounded layer search.

use std::cmp::Ordering;

use super::metric::distance;
use super::prepare::{check, Control};
use super::queue::{Candidates, Queue};
use super::types::{HNSWIndex, NodeId};
use super::visited::Visited;
use crate::{read_control::StorageReadControl, StorageBackendResult};

#[derive(Debug, Clone, Copy)]
pub(super) struct Candidate {
    pub(super) distance: f32,
    pub(super) node_id: NodeId,
}

impl PartialEq for Candidate {
    fn eq(&self, other: &Self) -> bool {
        self.distance.to_bits() == other.distance.to_bits() && self.node_id == other.node_id
    }
}

impl Eq for Candidate {}

impl PartialOrd for Candidate {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Candidate {
    fn cmp(&self, other: &Self) -> Ordering {
        self.distance
            .total_cmp(&other.distance)
            .then_with(|| self.node_id.cmp(&other.node_id))
    }
}

impl HNSWIndex {
    pub(super) fn greedy_search_layer(
        &self,
        query: &[f32],
        entry: NodeId,
        layer: usize,
        control: Control<'_>,
    ) -> StorageBackendResult<NodeId> {
        check(control)?;
        let Some(entry_node) = self.normalized_vectors.get(u128::from(entry))? else {
            return Ok(entry);
        };
        let mut best = Candidate {
            distance: distance(query, &entry_node.values),
            node_id: entry,
        };
        drop(entry_node);
        loop {
            check(control)?;
            let mut improved = false;
            let Some(node) = self.node(best.node_id)? else {
                return Ok(best.node_id);
            };
            let Some(neighbors) = node.neighbors.get(layer) else {
                return Ok(best.node_id);
            };
            for &neighbor_id in neighbors {
                check(control)?;
                let Some(neighbor) = self.normalized_vectors.get(u128::from(neighbor_id))? else {
                    continue;
                };
                let candidate = Candidate {
                    distance: distance(query, &neighbor.values),
                    node_id: neighbor_id,
                };
                if candidate < best {
                    best = candidate;
                    improved = true;
                }
            }
            if !improved {
                return Ok(best.node_id);
            }
        }
    }

    pub(super) fn search_layer(
        &self,
        query: &[f32],
        entries: &[NodeId],
        ef: usize,
        layer: usize,
        control: Control<'_>,
        workspace: Control<'_>,
    ) -> StorageBackendResult<Candidates> {
        check(control)?;
        let fallback = StorageReadControl::new(&self.memory, &uqa_core::CancellationToken::new());
        let workspace = workspace.unwrap_or(&fallback);
        let ef = ef.max(1);
        let mut visited = Visited::new(
            self.nodes.next_key(None, &self.memory)?.map(|id| id as u64),
            self.next_node_id.checked_sub(1),
            ef.saturating_mul(2).min(self.nodes.len()),
            Some(workspace),
        )?;
        let mut candidates = Queue::<true>::new(workspace);
        let mut nearest = Queue::<false>::new(workspace);
        for entry in entries {
            check(control)?;
            let Some(node) = self.normalized_vectors.get(u128::from(*entry))? else {
                continue;
            };
            if !visited.insert(*entry)? {
                continue;
            }
            let candidate = Candidate {
                distance: distance(query, &node.values),
                node_id: *entry,
            };
            candidates.push(candidate)?;
            nearest.push(candidate)?;
        }
        while let Some(current) = candidates.pop()? {
            check(control)?;
            if nearest.len() >= ef && nearest.peek()?.is_some_and(|worst| current > worst) {
                break;
            }
            let Some(node) = self.node(current.node_id)? else {
                continue;
            };
            let Some(neighbors) = node.neighbors.get(layer) else {
                continue;
            };
            for &neighbor_id in neighbors {
                check(control)?;
                if !visited.insert(neighbor_id)? {
                    continue;
                }
                let Some(neighbor) = self.normalized_vectors.get(u128::from(neighbor_id))? else {
                    continue;
                };
                let candidate = Candidate {
                    distance: distance(query, &neighbor.values),
                    node_id: neighbor_id,
                };
                if nearest.len() < ef || nearest.peek()?.is_some_and(|worst| candidate < worst) {
                    candidates.push(candidate)?;
                    nearest.push(candidate)?;
                    if nearest.len() > ef {
                        nearest.pop()?;
                    }
                }
            }
        }
        check(control)?;
        Ok(nearest.into_sorted())
    }

    pub(super) fn query_candidates(
        &self,
        query: &[f32],
        ef: usize,
        control: Control<'_>,
    ) -> StorageBackendResult<Candidates> {
        check(control)?;
        let Some(mut entry) = self.entry_point else {
            let fallback =
                StorageReadControl::new(&self.memory, &uqa_core::CancellationToken::new());
            return Ok(Queue::<false>::new(control.unwrap_or(&fallback)).into_sorted());
        };
        for layer in (1..=self.max_level).rev() {
            entry = self.greedy_search_layer(query, entry, layer, control)?;
        }
        self.search_layer(query, &[entry], ef, 0, control, control)
    }
}
