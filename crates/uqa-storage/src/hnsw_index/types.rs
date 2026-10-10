//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! HNSW graph state and construction parameters.

use super::store::Map;
use uqa_core::{memory::MemoryBudget, DocId};

use crate::vector_index::HNSWIndexParams;
use crate::StorageBackendResult;

pub(super) type NodeId = u64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct HNSWNode {
    pub(super) id: NodeId,
    pub(super) doc_id: DocId,
    pub(super) vector_ordinal: u32,
    pub(super) level: usize,
    pub(super) deleted: bool,
    pub(super) neighbors: Vec<Vec<NodeId>>,
}

#[derive(Debug, Clone)]
pub(super) struct HNSWVector {
    pub(super) values: Vec<f32>,
    pub(super) norm: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HNSWNodeSnapshot {
    pub node_id: NodeId,
    pub doc_id: DocId,
    pub vector_ordinal: u32,
    pub raw_vector: Vec<f32>,
    pub level: usize,
    pub deleted: bool,
    pub neighbors: Vec<Vec<NodeId>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HNSWGraphMeta {
    pub entry_point: Option<NodeId>,
    pub max_level: usize,
    pub next_node_id: NodeId,
    pub live_count: usize,
    pub deleted_count: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HNSWPersistenceDelta {
    pub meta: HNSWGraphMeta,
    pub nodes: Vec<HNSWNodeSnapshot>,
    pub full_rewrite: bool,
}

#[derive(Debug, Clone)]
pub struct HNSWIndex {
    pub(super) dimensions: u32,
    pub(super) params: HNSWIndexParams,
    pub(super) nodes: Map<HNSWNode>,
    pub(super) raw_vectors: Map<HNSWVector>,
    pub(super) normalized_vectors: Map<HNSWVector>,
    pub(super) active: Map<NodeId>,
    pub(super) entry_point: Option<NodeId>,
    pub(super) max_level: usize,
    pub(super) next_node_id: NodeId,
    pub(super) deleted_count: usize,
    pub(super) dirty_nodes: Map<u64>,
    pub(super) full_rewrite: bool,
    pub(super) memory: MemoryBudget,
}

impl HNSWIndex {
    pub fn new(dimensions: u32) -> Self {
        Self::with_params(dimensions, HNSWIndexParams::default())
            .expect("default HNSW parameters are valid")
    }

    pub fn with_params(dimensions: u32, params: HNSWIndexParams) -> StorageBackendResult<Self> {
        Self::with_memory(
            dimensions,
            params,
            &MemoryBudget::new(crate::mvcc::VersionedSessionOptions::default().retained_bytes),
        )
    }

    pub(super) fn with_memory(
        dimensions: u32,
        params: HNSWIndexParams,
        memory: &MemoryBudget,
    ) -> StorageBackendResult<Self> {
        let params = params.validate()?;
        Ok(Self {
            dimensions,
            params,
            nodes: Map::new(memory, memory.limit() / 16),
            raw_vectors: Map::new(memory, memory.limit() / 32),
            normalized_vectors: Map::new(memory, memory.limit() / 32),
            active: Map::new(memory, memory.limit() / 32),
            entry_point: None,
            max_level: 0,
            next_node_id: 1,
            deleted_count: 0,
            dirty_nodes: Map::new(memory, memory.limit() / 32),
            full_rewrite: true,
            memory: memory.clone(),
        })
    }

    pub fn params(&self) -> HNSWIndexParams {
        self.params
    }

    pub(super) fn max_connections(&self, layer: usize) -> usize {
        if layer == 0 {
            self.params.m.saturating_mul(2)
        } else {
            self.params.m
        }
    }
}

pub(super) fn active_key(document: DocId, ordinal: u32) -> u128 {
    (u128::from(document) << 32) | u128::from(ordinal)
}
