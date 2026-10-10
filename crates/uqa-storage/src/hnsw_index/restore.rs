//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Checked reconstruction accepts one persisted node or edge at a time.

use super::metric::{normalize_with_norm, MAX_HNSW_LEVEL};
use super::store::Record;
use super::types::{active_key, HNSWGraphMeta, HNSWIndex, HNSWNode, HNSWNodeSnapshot};
use crate::vector_index::{validate_vector_values, HNSWIndexParams};
use crate::{read_control::StorageReadControl, StorageBackendError, StorageBackendResult};
use uqa_core::memory::{Budgeted, MemoryError, MemoryReservation};

impl HNSWIndex {
    pub fn from_persistence(
        dimensions: u32,
        params: HNSWIndexParams,
        meta: HNSWGraphMeta,
        snapshots: Vec<HNSWNodeSnapshot>,
    ) -> StorageBackendResult<Self> {
        let control = StorageReadControl::with_limit(
            crate::mvcc::VersionedSessionOptions::default().retained_bytes,
        );
        Ok(
            Self::from_persistence_controlled(dimensions, params, meta, snapshots, &control)?
                .into_parts()
                .0,
        )
    }

    pub fn from_persistence_controlled(
        dimensions: u32,
        params: HNSWIndexParams,
        meta: HNSWGraphMeta,
        snapshots: Vec<HNSWNodeSnapshot>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Budgeted<Self>> {
        let mut builder = HNSWRestoreBuilder::new(dimensions, params, meta, control)?;
        for node in snapshots {
            builder.push(node)?;
        }
        builder.finish()
    }
}

pub struct HNSWRestoreBuilder {
    index: HNSWIndex,
    expected: HNSWGraphMeta,
    control: StorageReadControl,
    memory: MemoryReservation,
    pending: Option<(HNSWNode, MemoryReservation)>,
    failed: bool,
}

impl HNSWRestoreBuilder {
    pub fn new(
        dimensions: u32,
        params: HNSWIndexParams,
        meta: HNSWGraphMeta,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        if meta.max_level > MAX_HNSW_LEVEL {
            return Err(corrupt(&format!(
                "metadata level {} exceeds the supported maximum {MAX_HNSW_LEVEL}",
                meta.max_level
            )));
        }
        let memory = control.memory().reserve(size_of::<HNSWIndex>())?;
        let mut index = HNSWIndex::with_memory(dimensions, params, control.memory())?;
        index.entry_point = meta.entry_point;
        index.max_level = meta.max_level;
        index.next_node_id = meta.next_node_id;
        index.full_rewrite = false;
        Ok(Self {
            index,
            expected: meta,
            control: control.clone(),
            memory,
            pending: None,
            failed: false,
        })
    }

    pub fn push(&mut self, snapshot: HNSWNodeSnapshot) -> StorageBackendResult<()> {
        self.check_usable()?;
        let result = self.push_inner(snapshot);
        self.failed = result.is_err();
        result
    }

    fn push_inner(&mut self, snapshot: HNSWNodeSnapshot) -> StorageBackendResult<()> {
        self.control.check()?;
        self.flush_edges()?;
        let mut bytes = size_of::<HNSWNodeSnapshot>()
            .checked_add(
                snapshot
                    .raw_vector
                    .capacity()
                    .checked_mul(size_of::<f32>())
                    .ok_or(MemoryError::SizeOverflow)?,
            )
            .and_then(|n| {
                n.checked_add(
                    snapshot
                        .neighbors
                        .capacity()
                        .checked_mul(size_of::<Vec<u64>>())?,
                )
            })
            .ok_or(MemoryError::SizeOverflow)?;
        for layer in &snapshot.neighbors {
            bytes = bytes
                .checked_add(
                    layer
                        .capacity()
                        .checked_mul(size_of::<u64>())
                        .ok_or(MemoryError::SizeOverflow)?,
                )
                .ok_or(MemoryError::SizeOverflow)?;
        }
        let _input = self.control.memory().reserve(bytes)?;
        validate_vector_values(self.index.dimensions, &snapshot.raw_vector)?;
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
        for (layer, neighbors) in snapshot.neighbors.iter().enumerate() {
            if neighbors.len() > self.index.max_connections(layer) {
                return Err(corrupt(&format!(
                    "node {} layer {layer} exceeds the degree bound",
                    snapshot.node_id
                )));
            }
        }
        if self.index.node(snapshot.node_id)?.is_some() {
            return Err(corrupt(&format!("duplicate node id {}", snapshot.node_id)));
        }
        let key = active_key(snapshot.doc_id, snapshot.vector_ordinal);
        if !snapshot.deleted && self.index.active.get(key)?.is_some() {
            return Err(corrupt(&format!(
                "duplicate live vector {}:{}",
                snapshot.doc_id, snapshot.vector_ordinal
            )));
        }
        let _normalized = self.control.memory().reserve(
            snapshot
                .raw_vector
                .len()
                .checked_mul(size_of::<f32>())
                .ok_or(MemoryError::SizeOverflow)?,
        )?;
        let (normalized_vector, norm) = normalize_with_norm(&snapshot.raw_vector);
        self.index.put_vectors(
            snapshot.node_id,
            snapshot.raw_vector,
            normalized_vector,
            norm,
            Some(&self.control),
        )?;
        let node = HNSWNode {
            id: snapshot.node_id,
            doc_id: snapshot.doc_id,
            vector_ordinal: snapshot.vector_ordinal,
            level: snapshot.level,
            deleted: snapshot.deleted,
            neighbors: snapshot.neighbors,
        };
        if node.deleted {
            self.index.deleted_count = self
                .index
                .deleted_count
                .checked_add(1)
                .ok_or_else(|| corrupt("deleted-node counter overflow"))?;
        } else {
            self.index
                .active
                .insert(key, node.id, Some(&self.control))?;
        }
        self.index
            .nodes
            .insert(u128::from(node.id), node, Some(&self.control))
    }

    /// Providers with a separate edge relation append each decoded edge to its original source node. Retain one charged source until it changes, so ordered edge streams rewrite each spilled adjacency record only once. Unordered streams remain valid and flush on each source change.
    pub fn edge(&mut self, source: u64, layer: usize, target: u64) -> StorageBackendResult<()> {
        self.check_usable()?;
        let result = self.edge_inner(source, layer, target);
        self.failed = result.is_err();
        result
    }

    fn edge_inner(&mut self, source: u64, layer: usize, target: u64) -> StorageBackendResult<()> {
        self.control.check()?;
        if self
            .pending
            .as_ref()
            .is_none_or(|(node, _)| node.id != source)
        {
            self.flush_edges()?;
            let node = self
                .index
                .node(source)?
                .ok_or_else(|| corrupt(&format!("edge source {source} is missing")))?;
            let mut bytes = node.memory_bytes()?;
            for layer in 0..node.neighbors.len() {
                bytes = bytes
                    .checked_add(
                        self.index
                            .max_connections(layer)
                            .checked_mul(size_of::<u64>())
                            .ok_or(MemoryError::SizeOverflow)?,
                    )
                    .ok_or(MemoryError::SizeOverflow)?;
            }
            let mut memory = self.control.memory().reserve(bytes)?;
            let mut owned = (*node).clone();
            drop(node);
            for (layer, neighbors) in owned.neighbors.iter_mut().enumerate() {
                neighbors.reserve_exact(self.index.max_connections(layer) - neighbors.len());
            }
            let actual = owned.memory_bytes()?;
            if actual > memory.bytes() {
                memory.grow(actual - memory.bytes())?;
            }
            self.pending = Some((owned, memory));
        }
        let (node, _) = self.pending.as_mut().expect("selected edge source");
        let neighbors = node.neighbors.get(layer).ok_or_else(|| {
            corrupt(&format!(
                "node {source} has an edge at layer {layer} above level {}",
                node.level
            ))
        })?;
        if neighbors.len() >= self.index.max_connections(layer) {
            return Err(corrupt(&format!(
                "node {source} layer {layer} exceeds the degree bound"
            )));
        }
        node.neighbors[layer].push(target);
        Ok(())
    }

    fn flush_edges(&mut self) -> StorageBackendResult<()> {
        if let Some((node, _memory)) = self.pending.take() {
            // Restoration does not produce a persistence delta. In particular,
            // it need not build and spill a redundant dirty-node map.
            self.index
                .nodes
                .insert(u128::from(node.id), node, Some(&self.control))?;
        }
        Ok(())
    }

    pub fn finish(mut self) -> StorageBackendResult<Budgeted<HNSWIndex>> {
        self.check_usable()?;
        self.flush_edges()?;
        if self.expected.live_count != self.index.active.len()
            || self.expected.deleted_count != self.index.deleted_count
        {
            return Err(corrupt(&format!(
                "counter mismatch: metadata live/deleted={}/{}, graph={}/{}",
                self.expected.live_count,
                self.expected.deleted_count,
                self.index.active.len(),
                self.index.deleted_count
            )));
        }
        let mut previous = None;
        let mut next_ordinal = 0_u64;
        for entry in self.index.active.iter() {
            self.control.check()?;
            let (key, _) = entry?;
            let document = (key >> 32) as u64;
            if previous != Some(document) {
                previous = Some(document);
                next_ordinal = 0;
            }
            if u64::from(key as u32) != next_ordinal {
                return Err(corrupt("live document vector ordinals are not contiguous"));
            }
            next_ordinal += 1;
        }
        self.index.validate_controlled(Some(&self.control))?;
        self.index.dirty_nodes.clear();
        Ok(Budgeted::new(self.index, self.memory))
    }

    fn check_usable(&self) -> StorageBackendResult<()> {
        self.control.check()?;
        if self.failed {
            return Err(corrupt("restoration cannot continue after an error"));
        }
        Ok(())
    }
}

fn corrupt(message: &str) -> StorageBackendError {
    StorageBackendError::Other(format!("corrupt HNSW graph: {message}"))
}
