//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Versioned HNSW graph encoding, restoration, and dirty-node persistence.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use uqa_core::DocId;

use super::codec::{decode_value, encode_value, other_error, read_u64, usize_to_u64};
use super::index_keys::{hnsw_metadata_key, hnsw_node_key, hnsw_node_prefix};
use super::{KeyValueBatch, KeyValueStore, KeyValueVectorIndex};
use crate::hnsw_index::{
    HNSWGraphMeta, HNSWIndex, HNSWNodeSnapshot, HNSWPersistenceDelta, MAX_HNSW_LEVEL,
};
use crate::vector_index::HNSWIndexParams;
use crate::{StorageBackendError, StorageBackendResult};

const HNSW_FORMAT_VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
struct PersistedHNSWMetadata {
    format_version: u32,
    dimensions: u32,
    m: u64,
    ef_construction: u64,
    ef_search: u64,
    rebuild_threshold: u64,
    seed: u64,
    entry_point: Option<u64>,
    max_level: u64,
    next_node_id: u64,
    live_count: u64,
    deleted_count: u64,
    revision: u64,
}

#[derive(Debug, Serialize, Deserialize)]
struct PersistedHNSWNode {
    node_id: u64,
    doc_id: u64,
    vector_ordinal: u32,
    raw_vector: Vec<f32>,
    level: u64,
    deleted: bool,
    neighbors: Vec<Vec<u64>>,
}

pub(super) fn restore_graph(
    store: &dyn KeyValueStore,
    raw: &KeyValueVectorIndex,
    table: &str,
    field: &str,
    dimensions: u32,
    params: HNSWIndexParams,
) -> StorageBackendResult<(HNSWIndex, u64)> {
    let metadata = load_metadata(store, table, field)?.ok_or_else(|| {
        other_error(format!(
            "missing persisted HNSW metadata for {table}.{field}"
        ))
    })?;
    validate_metadata(&metadata, table, field, dimensions, params)?;
    let nodes = load_nodes(store, table, field)?;
    validate_canonical_vectors(&raw.load_all_with_ordinals()?, &nodes)?;
    let meta = HNSWGraphMeta {
        entry_point: metadata.entry_point,
        max_level: checked_level(metadata.max_level, "HNSW max_level")?,
        next_node_id: metadata.next_node_id,
        live_count: checked_usize(metadata.live_count, "HNSW live_count")?,
        deleted_count: checked_usize(metadata.deleted_count, "HNSW deleted_count")?,
    };
    let graph = HNSWIndex::from_persistence(dimensions, params, meta, nodes)?;
    Ok((graph, metadata.revision))
}

pub(super) fn load_revision(
    store: &dyn KeyValueStore,
    table: &str,
    field: &str,
) -> StorageBackendResult<Option<u64>> {
    Ok(load_metadata(store, table, field)?.map(|metadata| metadata.revision))
}

pub(super) fn stage_delta(
    batch: &mut dyn KeyValueBatch,
    table: &str,
    field: &str,
    dimensions: u32,
    params: HNSWIndexParams,
    delta: &HNSWPersistenceDelta,
    revision: u64,
) -> StorageBackendResult<()> {
    batch.put(
        &hnsw_metadata_key(table, field)?,
        &encode_value(&metadata_from_graph(
            dimensions, params, delta.meta, revision,
        )?)?,
    )?;
    if delta.full_rewrite {
        batch.delete_prefix(&hnsw_node_prefix(table, field)?)?;
    }
    for node in &delta.nodes {
        batch.put(
            &hnsw_node_key(table, field, node.node_id)?,
            &encode_value(&PersistedHNSWNode::try_from(node)?)?,
        )?;
    }
    Ok(())
}

fn load_metadata(
    store: &dyn KeyValueStore,
    table: &str,
    field: &str,
) -> StorageBackendResult<Option<PersistedHNSWMetadata>> {
    store
        .get(&hnsw_metadata_key(table, field)?)?
        .map(|bytes| decode_value(&bytes))
        .transpose()
}

fn load_nodes(
    store: &dyn KeyValueStore,
    table: &str,
    field: &str,
) -> StorageBackendResult<Vec<HNSWNodeSnapshot>> {
    let prefix = hnsw_node_prefix(table, field)?;
    let mut nodes = Vec::new();
    for (key, value) in store.scan_prefix(&prefix)? {
        let mut offset = prefix.len();
        let node_id = read_u64(&key, &mut offset)?;
        if offset != key.len() {
            return Err(other_error("persisted HNSW node key has trailing bytes"));
        }
        let persisted: PersistedHNSWNode = decode_value(&value)?;
        if persisted.node_id != node_id {
            return Err(other_error(format!(
                "persisted HNSW node key {node_id} disagrees with its payload {}",
                persisted.node_id
            )));
        }
        nodes.push(persisted.try_into()?);
    }
    Ok(nodes)
}

fn metadata_from_graph(
    dimensions: u32,
    params: HNSWIndexParams,
    meta: HNSWGraphMeta,
    revision: u64,
) -> StorageBackendResult<PersistedHNSWMetadata> {
    Ok(PersistedHNSWMetadata {
        format_version: HNSW_FORMAT_VERSION,
        dimensions,
        m: usize_to_u64(params.m, "HNSW m")?,
        ef_construction: usize_to_u64(params.ef_construction, "HNSW ef_construction")?,
        ef_search: usize_to_u64(params.ef_search, "HNSW ef_search")?,
        rebuild_threshold: usize_to_u64(params.rebuild_threshold, "HNSW rebuild_threshold")?,
        seed: params.seed,
        entry_point: meta.entry_point,
        max_level: usize_to_u64(meta.max_level, "HNSW max_level")?,
        next_node_id: meta.next_node_id,
        live_count: usize_to_u64(meta.live_count, "HNSW live_count")?,
        deleted_count: usize_to_u64(meta.deleted_count, "HNSW deleted_count")?,
        revision,
    })
}

fn validate_metadata(
    metadata: &PersistedHNSWMetadata,
    table: &str,
    field: &str,
    dimensions: u32,
    params: HNSWIndexParams,
) -> StorageBackendResult<()> {
    let persisted_params = HNSWIndexParams {
        m: checked_usize(metadata.m, "HNSW m")?,
        ef_construction: checked_usize(metadata.ef_construction, "HNSW ef_construction")?,
        ef_search: checked_usize(metadata.ef_search, "HNSW ef_search")?,
        rebuild_threshold: checked_usize(metadata.rebuild_threshold, "HNSW rebuild_threshold")?,
        seed: metadata.seed,
    };
    if metadata.format_version != HNSW_FORMAT_VERSION
        || metadata.dimensions != dimensions
        || persisted_params != params
    {
        return Err(other_error(format!(
            "persisted HNSW metadata does not match the catalog for {table}.{field}"
        )));
    }
    Ok(())
}

fn validate_canonical_vectors(
    canonical: &[(DocId, u32, Vec<f32>)],
    nodes: &[HNSWNodeSnapshot],
) -> StorageBackendResult<()> {
    let mut live = BTreeMap::<(DocId, u32), &[f32]>::new();
    for node in nodes.iter().filter(|node| !node.deleted) {
        if live
            .insert((node.doc_id, node.vector_ordinal), &node.raw_vector)
            .is_some()
        {
            return Err(corrupt(format!(
                "duplicate live graph vector {}:{}",
                node.doc_id, node.vector_ordinal
            )));
        }
    }
    for (doc_id, ordinal, vector) in canonical {
        let graph_vector = live.remove(&(*doc_id, *ordinal)).ok_or_else(|| {
            corrupt(format!(
                "canonical vector {doc_id}:{ordinal} has no live graph node"
            ))
        })?;
        if !same_bits(graph_vector, vector) {
            return Err(corrupt(format!(
                "canonical vector {doc_id}:{ordinal} differs from its live graph node"
            )));
        }
    }
    if let Some(((doc_id, ordinal), _)) = live.first_key_value() {
        return Err(corrupt(format!(
            "live graph node {doc_id}:{ordinal} has no canonical vector"
        )));
    }
    Ok(())
}

fn same_bits(left: &[f32], right: &[f32]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left, right)| left.to_bits() == right.to_bits())
}

fn checked_level(value: u64, field: &str) -> StorageBackendResult<usize> {
    let value = checked_usize(value, field)?;
    if value > MAX_HNSW_LEVEL {
        return Err(corrupt(format!(
            "{field} {value} exceeds supported maximum {MAX_HNSW_LEVEL}"
        )));
    }
    Ok(value)
}

fn checked_usize(value: u64, field: &str) -> StorageBackendResult<usize> {
    usize::try_from(value).map_err(|_| other_error(format!("{field} exceeds usize")))
}

fn corrupt(message: impl std::fmt::Display) -> StorageBackendError {
    StorageBackendError::Other(format!("corrupt HNSW graph: {message}"))
}

impl TryFrom<&HNSWNodeSnapshot> for PersistedHNSWNode {
    type Error = StorageBackendError;

    fn try_from(node: &HNSWNodeSnapshot) -> Result<Self, Self::Error> {
        Ok(Self {
            node_id: node.node_id,
            doc_id: node.doc_id,
            vector_ordinal: node.vector_ordinal,
            raw_vector: node.raw_vector.clone(),
            level: usize_to_u64(node.level, "HNSW node level")?,
            deleted: node.deleted,
            neighbors: node.neighbors.clone(),
        })
    }
}

impl TryFrom<PersistedHNSWNode> for HNSWNodeSnapshot {
    type Error = StorageBackendError;

    fn try_from(node: PersistedHNSWNode) -> Result<Self, Self::Error> {
        let level = checked_level(node.level, "HNSW node level")?;
        if node.neighbors.len() != level + 1 {
            return Err(corrupt(format!(
                "node {} has level {level} but {} adjacency layers",
                node.node_id,
                node.neighbors.len()
            )));
        }
        Ok(Self {
            node_id: node.node_id,
            doc_id: node.doc_id,
            vector_ordinal: node.vector_ordinal,
            raw_vector: node.raw_vector,
            level,
            deleted: node.deleted,
            neighbors: node.neighbors,
        })
    }
}
