//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Diversity-aware neighbor selection and reciprocal degree pruning.

use super::metric::distance;
use super::prepare::{check, Control};
use super::search::Candidate;
use super::store::Read;
use super::types::{HNSWIndex, HNSWVector, NodeId};
use crate::StorageBackendResult;
use uqa_core::memory::{Budgeted, BudgetedVec, MemoryError};

type DecodedVectors = BudgetedVec<(NodeId, Budgeted<HNSWVector>)>;

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
        drop(node);
        self.prune_neighbors(node_id, layer, current, control)
    }

    pub(super) fn connect_and_prune_node(
        &mut self,
        node_id: NodeId,
        neighbor_id: NodeId,
        layer: usize,
        control: Control<'_>,
    ) -> StorageBackendResult<()> {
        check(control)?;
        let Some(node) = self.node(node_id)? else {
            return Ok(());
        };
        let mut current = node.neighbors[layer].clone();
        if !current.contains(&neighbor_id) {
            current.push(neighbor_id);
        }
        drop(node);
        self.prune_neighbors(node_id, layer, current, control)
    }

    fn prune_neighbors(
        &mut self,
        node_id: NodeId,
        layer: usize,
        current: Vec<NodeId>,
        control: Control<'_>,
    ) -> StorageBackendResult<()> {
        let limit = self.max_connections(layer);
        // On a valid graph these are distinct, non-self neighbors with vectors.
        // If they all fit, diversity selection followed by rejected-candidate
        // filling retains every one; only the final identity sort can change.
        if current.len() <= limit {
            let mut selected = current;
            selected.sort_unstable();
            return self.modify_node(node_id, control, |node| {
                node.neighbors[layer] = selected;
            });
        }
        let protected = current
            .iter()
            .copied()
            .filter(|neighbor_id| layer == 0 && node_id.abs_diff(*neighbor_id) == 1)
            .collect::<Vec<_>>();
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
        let mut decoded = self.decoded_vectors();
        for node_id in candidates {
            check(control)?;
            if Some(node_id) != exclude {
                if let Some(node) = self.normalized_vectors.get(u128::from(node_id))? {
                    scored.push(Candidate {
                        distance: distance(query, &node.values),
                        node_id,
                    });
                    if !decoded.iter().any(|(id, _)| *id == node_id) {
                        retain_vector(&mut decoded, node_id, &node)?;
                    }
                }
            }
        }
        let mut candidates = scored;
        candidates.sort();
        candidates.dedup_by_key(|candidate| candidate.node_id);
        self.select_cached_candidates(
            candidates.into_iter().map(Ok),
            limit,
            exclude,
            control,
            decoded,
        )
    }

    pub(super) fn select_candidates(
        &self,
        candidates: impl IntoIterator<Item = StorageBackendResult<Candidate>>,
        limit: usize,
        exclude: Option<NodeId>,
        control: Control<'_>,
    ) -> StorageBackendResult<Vec<NodeId>> {
        self.select_cached_candidates(candidates, limit, exclude, control, self.decoded_vectors())
    }

    fn decoded_vectors(&self) -> DecodedVectors {
        let memory = self
            .memory
            .child((self.memory.limit() / 64).min(self.memory.available() / 4));
        BudgetedVec::new(&memory)
    }

    fn select_cached_candidates(
        &self,
        candidates: impl IntoIterator<Item = StorageBackendResult<Candidate>>,
        limit: usize,
        exclude: Option<NodeId>,
        control: Control<'_>,
        mut decoded: DecodedVectors,
    ) -> StorageBackendResult<Vec<NodeId>> {
        let mut selected = Vec::new();
        let mut rejected = Vec::new();
        // A selected vector is compared with many later candidates. Retain a
        // bounded set of decoded values for this read-only selection; resident
        // vectors can already be borrowed. Optional admission never consumes
        // the allowance needed by the ordinary two-vector comparison.
        for candidate in candidates {
            check(control)?;
            if selected.len() == limit {
                break;
            }
            let candidate = candidate?;
            if Some(candidate.node_id) == exclude {
                continue;
            }
            let cached = decoded.iter().find(|(id, _)| *id == candidate.node_id);
            let loaded = if cached.is_none() {
                self.normalized_vectors.get(u128::from(candidate.node_id))?
            } else {
                None
            };
            let Some(candidate_node) = cached.map(|(_, value)| &**value).or(loaded.as_deref())
            else {
                continue;
            };
            let mut diverse = true;
            for selected_id in &selected {
                check(control)?;
                if let Some((_, selected_node)) = decoded.iter().find(|(id, _)| id == selected_id) {
                    let separated = distance(&candidate_node.values, &selected_node.values)
                        > candidate.distance;
                    if !separated {
                        diverse = false;
                        break;
                    }
                    continue;
                }
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
                if let Some(loaded) = &loaded {
                    retain_vector(&mut decoded, candidate.node_id, loaded)?;
                }
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

fn retain_vector(
    decoded: &mut DecodedVectors,
    node_id: NodeId,
    vector: &Read<'_, HNSWVector>,
) -> StorageBackendResult<()> {
    if let Read::Owned(retained) = vector {
        let retain = || -> Result<Budgeted<HNSWVector>, MemoryError> {
            let memory = decoded.budget().reserve(retained.reserved_bytes())?;
            Ok(Budgeted::new((**vector).clone(), memory))
        };
        let result = retain().and_then(|value| decoded.push((node_id, value)));
        match result {
            Ok(()) | Err(MemoryError::Limit { .. }) => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}
