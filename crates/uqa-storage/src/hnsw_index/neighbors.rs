//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Diversity-aware neighbor selection and reciprocal degree pruning.

use super::metric::distance;
use super::prepare::{check, Control};
use super::search::Candidate;
use super::types::{HNSWIndex, NodeId};
use crate::StorageBackendResult;

impl HNSWIndex {
    pub(super) fn ensure_layer_zero_backbone(
        &self,
        node_id: NodeId,
        neighbors: &mut Vec<NodeId>,
    ) -> StorageBackendResult<()> {
        let Some(previous_id) = node_id.checked_sub(1) else {
            return Ok(());
        };
        if self.nodes.contains_key(u128::from(previous_id))? && !neighbors.contains(&previous_id) {
            neighbors.push(previous_id);
            neighbors.sort_unstable();
        }
        Ok(())
    }

    pub(super) fn prune_node(
        &mut self,
        node_id: NodeId,
        layer: usize,
        control: Control<'_>,
    ) -> StorageBackendResult<()> {
        check(control)?;
        let Some(node) = self.node(node_id)? else {
            return Ok(());
        };
        let Some(current) = node.neighbors.get(layer).cloned() else {
            return Ok(());
        };
        let protected = current
            .iter()
            .copied()
            .filter(|neighbor_id| layer == 0 && node_id.abs_diff(*neighbor_id) == 1)
            .collect::<Vec<_>>();
        let limit = self.max_connections(layer);
        let mut selected = protected.clone();
        selected.extend(
            self.select_neighbors(
                &self.normalized_vector(node_id)?.values,
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
        let removed = current
            .into_iter()
            .filter(|neighbor| selected.binary_search(neighbor).is_err())
            .collect::<Vec<_>>();
        drop(node);
        self.modify_node(node_id, control, |node| {
            node.neighbors[layer] = selected;
        })?;
        for removed_id in removed {
            check(control)?;
            self.modify_node(removed_id, control, |neighbor| {
                if let Some(reverse) = neighbor.neighbors.get_mut(layer) {
                    reverse.retain(|candidate| *candidate != node_id);
                }
            })?;
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
                if let Some(node) = self.normalized_vectors.get(u128::from(node_id))? {
                    scored.push(Candidate {
                        distance: distance(query, &node.values),
                        node_id,
                    });
                }
            }
        }
        let mut candidates = scored;
        candidates.sort();
        candidates.dedup_by_key(|candidate| candidate.node_id);
        self.select_candidates(candidates.into_iter().map(Ok), limit, exclude, control)
    }

    pub(super) fn select_candidates(
        &self,
        candidates: impl IntoIterator<Item = StorageBackendResult<Candidate>>,
        limit: usize,
        exclude: Option<NodeId>,
        control: Control<'_>,
    ) -> StorageBackendResult<Vec<NodeId>> {
        let mut selected = Vec::new();
        let mut rejected = Vec::new();
        for candidate in candidates {
            check(control)?;
            if selected.len() == limit {
                break;
            }
            let candidate = candidate?;
            if Some(candidate.node_id) == exclude {
                continue;
            }
            let Some(candidate_node) =
                self.normalized_vectors.get(u128::from(candidate.node_id))?
            else {
                continue;
            };
            let mut diverse = true;
            for selected_id in &selected {
                check(control)?;
                if let Some(selected_node) =
                    self.normalized_vectors.get(u128::from(*selected_id))?
                {
                    let separated = distance(&candidate_node.values, &selected_node.values)
                        > candidate.distance;
                    if !separated {
                        diverse = false;
                        break;
                    }
                }
            }
            if diverse && selected.len() < limit {
                selected.push(candidate.node_id);
            } else if rejected.len() < limit {
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
