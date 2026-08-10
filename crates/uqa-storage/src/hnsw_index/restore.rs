//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Checked reconstruction from persisted graph state.

use std::collections::{BTreeMap, BTreeSet};

use super::metric::{normalize_with_norm, MAX_HNSW_LEVEL};
use super::types::{HNSWGraphMeta, HNSWIndex, HNSWNode, HNSWNodeSnapshot};
use crate::vector_index::{validate_vector_values, HNSWIndexParams};
use crate::{StorageBackendError, StorageBackendResult};

impl HNSWIndex {
    pub(crate) fn from_persistence(
        dimensions: u32,
        params: HNSWIndexParams,
        meta: HNSWGraphMeta,
        snapshots: Vec<HNSWNodeSnapshot>,
    ) -> StorageBackendResult<Self> {
        let params = params.validate()?;
        if meta.max_level > MAX_HNSW_LEVEL {
            return Err(corrupt(&format!(
                "metadata level {} exceeds the supported maximum {MAX_HNSW_LEVEL}",
                meta.max_level
            )));
        }
        let mut nodes = BTreeMap::new();
        let mut active = BTreeMap::new();
        let mut deleted_count = 0_usize;
        for snapshot in snapshots {
            validate_vector_values(dimensions, &snapshot.raw_vector)?;
            if snapshot.level > MAX_HNSW_LEVEL {
                return Err(corrupt(&format!(
                    "node {} level {} exceeds the supported maximum {MAX_HNSW_LEVEL}",
                    snapshot.node_id, snapshot.level
                )));
            }
            if snapshot.neighbors.len() != snapshot.level + 1 {
                return Err(corrupt(&format!(
                    "node {} has level {} but {} adjacency layers",
                    snapshot.node_id,
                    snapshot.level,
                    snapshot.neighbors.len()
                )));
            }
            let (normalized_vector, norm) = normalize_with_norm(&snapshot.raw_vector);
            let node = HNSWNode {
                id: snapshot.node_id,
                doc_id: snapshot.doc_id,
                vector_ordinal: snapshot.vector_ordinal,
                norm,
                normalized_vector,
                raw_vector: snapshot.raw_vector,
                level: snapshot.level,
                deleted: snapshot.deleted,
                neighbors: snapshot.neighbors,
            };
            if nodes.insert(node.id, node.clone()).is_some() {
                return Err(corrupt(&format!("duplicate node id {}", node.id)));
            }
            if node.deleted {
                deleted_count = deleted_count
                    .checked_add(1)
                    .ok_or_else(|| corrupt("deleted-node counter overflow"))?;
            } else if active
                .insert((node.doc_id, node.vector_ordinal), node.id)
                .is_some()
            {
                return Err(corrupt(&format!(
                    "duplicate live vector {}:{}",
                    node.doc_id, node.vector_ordinal
                )));
            }
        }
        let index = Self {
            dimensions,
            params,
            nodes,
            active,
            entry_point: meta.entry_point,
            max_level: meta.max_level,
            next_node_id: meta.next_node_id,
            deleted_count,
            dirty_nodes: BTreeSet::new(),
            full_rewrite: false,
        };
        if meta.live_count != index.active.len() || meta.deleted_count != index.deleted_count {
            return Err(corrupt(&format!(
                "counter mismatch: metadata live/deleted={}/{}, graph={}/{}",
                meta.live_count,
                meta.deleted_count,
                index.active.len(),
                index.deleted_count
            )));
        }
        index.validate_invariants()?;
        Ok(index)
    }
}

fn corrupt(message: &str) -> StorageBackendError {
    StorageBackendError::Other(format!("corrupt HNSW graph: {message}"))
}
