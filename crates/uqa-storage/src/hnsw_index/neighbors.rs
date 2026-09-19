//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Diversity-aware neighbor selection and reciprocal degree pruning.

use std::collections::BTreeSet;

use super::metric::distance;
use super::prepare::{check, Control};
use super::search::Candidate;
use super::types::{HNSWIndex, NodeId};
use crate::StorageBackendResult;

impl HNSWIndex {
    pub(super) fn ensure_layer_zero_backbone(&self, node_id: NodeId, neighbors: &mut Vec<NodeId>) {
        let Some(previous_id) = node_id.checked_sub(1) else {
            return;
        };
        if self.nodes.contains_key(&previous_id) && !neighbors.contains(&previous_id) {
            neighbors.push(previous_id);
            neighbors.sort_unstable();
        }
    }

    pub(super) fn prune_node(
        &mut self,
        node_id: NodeId,
        layer: usize,
        control: Control<'_>,
    ) -> StorageBackendResult<()> {
        check(control)?;
        let Some(node) = self.nodes.get(&node_id) else {
            return Ok(());
        };
        let Some(current) = node.neighbors.get(layer).cloned() else {
            return Ok(());
        };
        let protected = current
            .iter()
            .copied()
            .filter(|neighbor_id| layer == 0 && node_id.abs_diff(*neighbor_id) == 1)
            .collect::<BTreeSet<_>>();
        let limit = self.max_connections(layer);
        let mut selected = protected.iter().copied().collect::<Vec<_>>();
        selected.extend(
            self.select_neighbors(
                &node.normalized_vector,
                current
                    .iter()
                    .copied()
                    .filter(|neighbor_id| !protected.contains(neighbor_id)),
                limit.saturating_sub(selected.len()),
                Some(node_id),
                control,
            )?,
        );
        selected.sort_unstable();
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
            check(control)?;
            if let Some(neighbor) = self.nodes.get_mut(&removed_id) {
                if let Some(reverse) = neighbor.neighbors.get_mut(layer) {
                    reverse.retain(|candidate| *candidate != node_id);
                    self.dirty_nodes.insert(removed_id);
                }
            }
        }
        Ok(())
    }

    pub(super) fn select_neighbors(
        &self,
        query: &[f32],
        candidates: impl IntoIterator<Item = NodeId>,
        limit: usize,
        exclude: Option<NodeId>,
        control: Control<'_>,
    ) -> StorageBackendResult<Vec<NodeId>> {
        check(control)?;
        let mut scored = Vec::new();
        for node_id in candidates {
            check(control)?;
            if Some(node_id) != exclude {
                if let Some(node) = self.nodes.get(&node_id) {
                    scored.push(Candidate {
                        distance: distance(query, &node.normalized_vector),
                        node_id,
                    });
                }
            }
        }
        let mut candidates = scored;
        candidates.sort();
        candidates.dedup_by_key(|candidate| candidate.node_id);
        let mut selected = Vec::with_capacity(limit.min(candidates.len()));
        let mut rejected = Vec::new();
        for candidate in candidates {
            check(control)?;
            let Some(candidate_node) = self.nodes.get(&candidate.node_id) else {
                continue;
            };
            let mut diverse = true;
            for selected_id in &selected {
                check(control)?;
                if let Some(selected_node) = self.nodes.get(selected_id) {
                    let separated = distance(
                        &candidate_node.normalized_vector,
                        &selected_node.normalized_vector,
                    ) > candidate.distance;
                    if !separated {
                        diverse = false;
                        break;
                    }
                }
            }
            if diverse && selected.len() < limit {
                selected.push(candidate.node_id);
            } else {
                rejected.push(candidate.node_id);
            }
        }
        for candidate in rejected {
            check(control)?;
            if selected.len() >= limit {
                break;
            }
            selected.push(candidate);
        }
        selected.sort_unstable();
        check(control)?;
        Ok(selected)
    }
}
