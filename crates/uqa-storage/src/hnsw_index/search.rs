//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Greedy hierarchy traversal and bounded layer search.

use std::cmp::{Ordering, Reverse};
use std::collections::{BTreeSet, BinaryHeap};

use super::metric::distance;
use super::types::{HNSWIndex, NodeId};

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
    pub(super) fn greedy_search_layer(&self, query: &[f32], entry: NodeId, layer: usize) -> NodeId {
        let Some(entry_node) = self.nodes.get(&entry) else {
            return entry;
        };
        let mut best = Candidate {
            distance: distance(query, &entry_node.normalized_vector),
            node_id: entry,
        };
        loop {
            let mut improved = false;
            let neighbors = self
                .nodes
                .get(&best.node_id)
                .and_then(|node| node.neighbors.get(layer))
                .cloned()
                .unwrap_or_default();
            for neighbor_id in neighbors {
                let Some(neighbor) = self.nodes.get(&neighbor_id) else {
                    continue;
                };
                let candidate = Candidate {
                    distance: distance(query, &neighbor.normalized_vector),
                    node_id: neighbor_id,
                };
                if candidate < best {
                    best = candidate;
                    improved = true;
                }
            }
            if !improved {
                return best.node_id;
            }
        }
    }

    pub(super) fn search_layer(
        &self,
        query: &[f32],
        entries: &[NodeId],
        ef: usize,
        layer: usize,
    ) -> Vec<Candidate> {
        let ef = ef.max(1);
        let mut visited = BTreeSet::new();
        let mut candidates = BinaryHeap::<Reverse<Candidate>>::new();
        let mut nearest = BinaryHeap::<Candidate>::new();
        for entry in entries {
            let Some(node) = self.nodes.get(entry) else {
                continue;
            };
            if !visited.insert(*entry) {
                continue;
            }
            let candidate = Candidate {
                distance: distance(query, &node.normalized_vector),
                node_id: *entry,
            };
            candidates.push(Reverse(candidate));
            nearest.push(candidate);
        }
        while let Some(Reverse(current)) = candidates.pop() {
            if nearest.len() >= ef && nearest.peek().is_some_and(|worst| current > *worst) {
                break;
            }
            let neighbors = self
                .nodes
                .get(&current.node_id)
                .and_then(|node| node.neighbors.get(layer))
                .cloned()
                .unwrap_or_default();
            for neighbor_id in neighbors {
                if !visited.insert(neighbor_id) {
                    continue;
                }
                let Some(neighbor) = self.nodes.get(&neighbor_id) else {
                    continue;
                };
                let candidate = Candidate {
                    distance: distance(query, &neighbor.normalized_vector),
                    node_id: neighbor_id,
                };
                if nearest.len() < ef || nearest.peek().is_some_and(|worst| candidate < *worst) {
                    candidates.push(Reverse(candidate));
                    nearest.push(candidate);
                    if nearest.len() > ef {
                        nearest.pop();
                    }
                }
            }
        }
        let mut result = nearest.into_vec();
        result.sort();
        result
    }

    pub(super) fn query_candidates(&self, query: &[f32], ef: usize) -> Vec<Candidate> {
        let Some(mut entry) = self.entry_point else {
            return Vec::new();
        };
        for layer in (1..=self.max_level).rev() {
            entry = self.greedy_search_layer(query, entry, layer);
        }
        self.search_layer(query, &[entry], ef, 0)
    }
}
