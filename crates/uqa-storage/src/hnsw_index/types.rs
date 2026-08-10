//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! HNSW graph state and construction parameters.

use std::collections::{BTreeMap, BTreeSet};

use uqa_core::DocId;

use crate::vector_index::HNSWIndexParams;
use crate::StorageBackendResult;

pub(super) type NodeId = u64;

#[derive(Debug, Clone)]
pub(super) struct HNSWNode {
    pub(super) id: NodeId,
    pub(super) doc_id: DocId,
    pub(super) vector_ordinal: u32,
    pub(super) raw_vector: Vec<f32>,
    pub(super) norm: f32,
    pub(super) normalized_vector: Vec<f32>,
    pub(super) level: usize,
    pub(super) deleted: bool,
    pub(super) neighbors: Vec<Vec<NodeId>>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct HNSWNodeSnapshot {
    pub node_id: NodeId,
    pub doc_id: DocId,
    pub vector_ordinal: u32,
    pub raw_vector: Vec<f32>,
    pub level: usize,
    pub deleted: bool,
    pub neighbors: Vec<Vec<NodeId>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HNSWGraphMeta {
    pub entry_point: Option<NodeId>,
    pub max_level: usize,
    pub next_node_id: NodeId,
    pub live_count: usize,
    pub deleted_count: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct HNSWPersistenceDelta {
    pub meta: HNSWGraphMeta,
    pub nodes: Vec<HNSWNodeSnapshot>,
    pub full_rewrite: bool,
}

#[derive(Debug, Clone)]
pub struct HNSWIndex {
    pub(super) dimensions: u32,
    pub(super) params: HNSWIndexParams,
    pub(super) nodes: BTreeMap<NodeId, HNSWNode>,
    pub(super) active: BTreeMap<(DocId, u32), NodeId>,
    pub(super) entry_point: Option<NodeId>,
    pub(super) max_level: usize,
    pub(super) next_node_id: NodeId,
    pub(super) deleted_count: usize,
    pub(super) dirty_nodes: BTreeSet<NodeId>,
    pub(super) full_rewrite: bool,
}

impl HNSWIndex {
    pub fn new(dimensions: u32) -> Self {
        Self::with_params(dimensions, HNSWIndexParams::default())
            .expect("default HNSW parameters are valid")
    }

    pub fn with_params(dimensions: u32, params: HNSWIndexParams) -> StorageBackendResult<Self> {
        let params = params.validate()?;
        Ok(Self {
            dimensions,
            params,
            nodes: BTreeMap::new(),
            active: BTreeMap::new(),
            entry_point: None,
            max_level: 0,
            next_node_id: 1,
            deleted_count: 0,
            dirty_nodes: BTreeSet::new(),
            full_rewrite: true,
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
