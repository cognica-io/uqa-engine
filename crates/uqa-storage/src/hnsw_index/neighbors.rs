//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Diversity-aware neighbor selection and reciprocal degree pruning.

use std::collections::BTreeSet;

use super::metric::distance;
use super::search::Candidate;
use super::types::{HNSWIndex, NodeId};

impl HNSWIndex {
    pub(super) fn prune_node(&mut self, node_id: NodeId, layer: usize) {
        let Some(node) = self.nodes.get(&node_id) else {
            return;
        };
        let Some(current) = node.neighbors.get(layer).cloned() else {
            return;
        };
        let selected = self.select_neighbors(
            &node.normalized_vector,
            current.iter().copied(),
            self.max_connections(layer),
            Some(node_id),
        );
        let selected_set = selected.iter().copied().collect::<BTreeSet<_>>();
        let removed = current
            .into_iter()
            .filter(|neighbor| !selected_set.contains(neighbor))
            .collect::<Vec<_>>();
        if let Some(node) = self.nodes.get_mut(&node_id) {
            node.neighbors[layer] = selected;
            self.dirty_nodes.insert(node_id);
        }
        for removed_id in removed {
            if let Some(neighbor) = self.nodes.get_mut(&removed_id) {
                if let Some(reverse) = neighbor.neighbors.get_mut(layer) {
                    reverse.retain(|candidate| *candidate != node_id);
                    self.dirty_nodes.insert(removed_id);
                }
            }
        }
    }

    pub(super) fn select_neighbors(
        &self,
        query: &[f32],
        candidates: impl IntoIterator<Item = NodeId>,
        limit: usize,
        exclude: Option<NodeId>,
    ) -> Vec<NodeId> {
        let mut candidates = candidates
            .into_iter()
            .filter(|node_id| Some(*node_id) != exclude)
            .filter_map(|node_id| {
                self.nodes.get(&node_id).map(|node| Candidate {
                    distance: distance(query, &node.normalized_vector),
                    node_id,
                })
            })
            .collect::<Vec<_>>();
        candidates.sort();
        candidates.dedup_by_key(|candidate| candidate.node_id);
        let mut selected = Vec::with_capacity(limit.min(candidates.len()));
        let mut rejected = Vec::new();
        for candidate in candidates {
            let Some(candidate_node) = self.nodes.get(&candidate.node_id) else {
                continue;
            };
            let diverse = selected.iter().all(|selected_id| {
                self.nodes.get(selected_id).is_none_or(|selected_node| {
                    distance(
                        &candidate_node.normalized_vector,
                        &selected_node.normalized_vector,
                    ) > candidate.distance
                })
            });
            if diverse && selected.len() < limit {
                selected.push(candidate.node_id);
            } else {
                rejected.push(candidate.node_id);
            }
        }
        for candidate in rejected {
            if selected.len() >= limit {
                break;
            }
            selected.push(candidate);
        }
        selected.sort_unstable();
        selected
    }
}
