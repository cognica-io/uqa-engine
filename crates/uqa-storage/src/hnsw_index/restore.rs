//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Checked reconstruction from persisted graph state.

use std::collections::{BTreeMap, BTreeSet};

use super::metric::{normalize_with_norm, MAX_HNSW_LEVEL};
use super::prepare::{check, Control};
use super::types::{HNSWGraphMeta, HNSWIndex, HNSWNode, HNSWNodeSnapshot};
use crate::vector_index::{validate_vector_values, HNSWIndexParams};
use crate::{read_control::StorageReadControl, StorageBackendError, StorageBackendResult};
use uqa_core::memory::{Budgeted, MemoryError};

impl HNSWIndex {
    pub fn from_persistence(
        dimensions: u32,
        params: HNSWIndexParams,
        meta: HNSWGraphMeta,
        snapshots: Vec<HNSWNodeSnapshot>,
    ) -> StorageBackendResult<Self> {
        Self::restore(dimensions, params, meta, snapshots, None)
    }

    /// Reserve the retained graph and validation workspace before reconstruction.
    pub fn from_persistence_controlled(
        dimensions: u32,
        params: HNSWIndexParams,
        meta: HNSWGraphMeta,
        snapshots: Vec<HNSWNodeSnapshot>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Budgeted<Self>> {
        control.check()?;
        let mut bytes = snapshots
            .len()
            .checked_mul(
                size_of::<(u64, HNSWNode)>()
                    + size_of::<((u64, u32), u64)>()
                    + 4 * size_of::<u64>(),
            )
            .ok_or(MemoryError::SizeOverflow)?;
        for node in &snapshots {
            control.check()?;
            let vectors = node
                .raw_vector
                .capacity()
                .checked_add(node.raw_vector.len())
                .and_then(|n| n.checked_mul(size_of::<f32>()))
                .ok_or(MemoryError::SizeOverflow)?;
            let layers = node
                .neighbors
                .capacity()
                .checked_mul(size_of::<Vec<u64>>())
                .ok_or(MemoryError::SizeOverflow)?;
            bytes = bytes
                .checked_add(vectors)
                .and_then(|n| n.checked_add(layers))
                .ok_or(MemoryError::SizeOverflow)?;
            for layer in &node.neighbors {
                control.check()?;
                bytes = bytes
                    .checked_add(
                        layer
                            .capacity()
                            .checked_add(layer.len())
                            .and_then(|n| n.checked_mul(size_of::<u64>()))
                            .ok_or(MemoryError::SizeOverflow)?,
                    )
                    .ok_or(MemoryError::SizeOverflow)?;
            }
        }
        let memory = control.memory().reserve(bytes)?;
        let index = Self::restore(dimensions, params, meta, snapshots, Some(control))?;
        Ok(Budgeted::new(index, memory))
    }

    fn restore(
        dimensions: u32,
        params: HNSWIndexParams,
        meta: HNSWGraphMeta,
        snapshots: Vec<HNSWNodeSnapshot>,
        control: Control<'_>,
    ) -> StorageBackendResult<Self> {
        check(control)?;
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
            check(control)?;
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
            let id = node.id;
            if nodes.insert(id, node).is_some() {
                return Err(corrupt(&format!("duplicate node id {id}")));
            }
        }
        let mut previous_document = None;
        let mut next_ordinal = 0_u64;
        for (document, ordinal) in active.keys() {
            check(control)?;
            if previous_document != Some(*document) {
                previous_document = Some(*document);
                next_ordinal = 0;
            }
            if u64::from(*ordinal) != next_ordinal {
                return Err(corrupt("live document vector ordinals are not contiguous"));
            }
            next_ordinal += 1;
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
        index.validate_controlled(control)?;
        Ok(index)
    }
}

fn corrupt(message: &str) -> StorageBackendError {
    StorageBackendError::Other(format!("corrupt HNSW graph: {message}"))
}
