//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical HNSW addressing and codecs; graph preparation remains provider independent.

mod resolve;
pub(super) use resolve::merge;

use super::VersionResult;
use crate::{
    hnsw_index::{HNSWGraphMeta, HNSWNodeSnapshot},
    read_control::StorageReadControl,
    HNSWIndexParams,
};
use uqa_core::{
    memory::{Budgeted, BudgetedVec},
    DocId,
};

#[derive(Clone, Copy)]
pub struct HNSWRecordHeader {
    pub dimensions: u32,
    pub params: HNSWIndexParams,
    pub meta: HNSWGraphMeta,
    pub revision: Option<u64>,
}

#[derive(Clone, Copy)]
pub enum HNSWRecordKey {
    Structure,
    Document(DocId),
    Vectors,
    Nodes,
    Node(u64),
    Edge {
        source: u64,
        layer: usize,
        target: u64,
    },
}

pub enum HNSWRecordValue<'a> {
    Header {
        meta: HNSWGraphMeta,
        revision: Option<u64>,
    },
    Node(&'a HNSWNodeSnapshot),
    Edge,
}

/// Providers implement physical keys and payloads. Common storage owns restoration, conflicts, ordered inputs and matching graph publication.
pub trait HNSWRecordLayout: Send + Sync {
    fn metadata_key(
        &self,
        key: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<Option<BudgetedVec<u8>>>;
    fn key(
        &self,
        metadata: &[u8],
        address: HNSWRecordKey,
        control: &StorageReadControl,
    ) -> VersionResult<BudgetedVec<u8>>;
    /// `None` means adjacency is embedded in each node. Otherwise select all edges or one source's edges.
    fn edges_prefix(
        &self,
        metadata: &[u8],
        source: Option<u64>,
        control: &StorageReadControl,
    ) -> VersionResult<Option<BudgetedVec<u8>>>;
    fn header(
        &self,
        key: &[u8],
        value: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<HNSWRecordHeader>;
    fn vector_id(&self, key: &[u8], control: &StorageReadControl) -> VersionResult<(DocId, u32)>;
    fn vector(
        &self,
        key: &[u8],
        value: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<(DocId, u32, BudgetedVec<f32>)>;
    /// Separate-edge layouts return one empty adjacency list for each declared layer.
    fn node(
        &self,
        key: &[u8],
        value: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<Budgeted<HNSWNodeSnapshot>>;
    fn edge(
        &self,
        key: &[u8],
        value: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<(u64, usize, u64)>;
    fn encode(
        &self,
        key: &[u8],
        template: &[u8],
        value: HNSWRecordValue<'_>,
        control: &StorageReadControl,
    ) -> VersionResult<BudgetedVec<u8>>;
}
