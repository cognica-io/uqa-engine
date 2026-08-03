//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Incremental multilayer graph construction.

use uqa_core::DocId;

use super::metric::{deterministic_level, normalize};
use super::search::Candidate;
use super::types::{HNSWIndex, HNSWNode};
use crate::{StorageBackendError, StorageBackendResult};

impl HNSWIndex {
    pub(super) fn insert_vector(
        &mut self,
        doc_id: DocId,
        vector_ordinal: u32,
        raw_vector: Vec<f32>,
    ) -> StorageBackendResult<()> {
        let node_id = self.next_node_id;
        self.next_node_id = self
            .next_node_id
            .checked_add(1)
            .ok_or_else(|| StorageBackendError::Other("HNSW node id space exhausted".into()))?;
        let level = deterministic_level(self.params.seed, node_id, self.params.m);
        let normalized_vector = normalize(&raw_vector);
        let previous_entry = self.entry_point;
        let previous_max_level = self.max_level;
        self.nodes.insert(
            node_id,
            HNSWNode {
                id: node_id,
                doc_id,
                vector_ordinal,
                raw_vector,
                normalized_vector: normalized_vector.clone(),
                level,
                deleted: false,
                neighbors: vec![Vec::new(); level + 1],
            },
        );
        self.active.insert((doc_id, vector_ordinal), node_id);
        self.dirty_nodes.insert(node_id);

        let Some(mut entry) = previous_entry else {
            self.entry_point = Some(node_id);
            self.max_level = level;
            return Ok(());
        };
        if previous_max_level > level {
            for layer in ((level + 1)..=previous_max_level).rev() {
                entry = self.greedy_search_layer(&normalized_vector, entry, layer);
            }
        }
        for layer in (0..=level.min(previous_max_level)).rev() {
            let candidates = self.search_layer(
                &normalized_vector,
                &[entry],
                self.params.ef_construction,
                layer,
            );
            let mut selected = self.select_neighbors(
                &normalized_vector,
                candidates.iter().map(|candidate| candidate.node_id),
                self.max_connections(layer),
                Some(node_id),
            );
            if layer == 0 {
                self.ensure_layer_zero_backbone(node_id, &mut selected);
            }
            if let Some(node) = self.nodes.get_mut(&node_id) {
                node.neighbors[layer].clone_from(&selected);
            }
            for neighbor_id in selected {
                if let Some(neighbor) = self.nodes.get_mut(&neighbor_id) {
                    if !neighbor.neighbors[layer].contains(&node_id) {
                        neighbor.neighbors[layer].push(node_id);
                    }
                    self.dirty_nodes.insert(neighbor_id);
                }
                self.prune_node(neighbor_id, layer);
            }
            self.prune_node(node_id, layer);
            if let Some(Candidate { node_id, .. }) = candidates.first() {
                entry = *node_id;
            }
        }
        if level > previous_max_level {
            self.entry_point = Some(node_id);
            self.max_level = level;
        }
        Ok(())
    }
}
