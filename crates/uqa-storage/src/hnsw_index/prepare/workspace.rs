//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Reserve live graph buffers and construction scratch before cloning or evaluating a candidate.

use super::{HNSWIndex, HNSWMutation, HNSWNodeSnapshot, StorageReadControl};
use crate::hnsw_index::{
    metric::{deterministic_level, MAX_HNSW_LEVEL},
    search::Candidate,
    types::{HNSWNode, NodeId},
};
use crate::{vector_index::validate_vector_values, StorageBackendResult};
use uqa_core::{
    memory::{MemoryError, MemoryReservation},
    DocId,
};

pub(super) fn validate_mutations(
    dimensions: u32,
    mutations: &[HNSWMutation<'_>],
    control: &StorageReadControl,
) -> StorageBackendResult<usize> {
    let mut additions = 0;
    for mutation in mutations {
        control.check()?;
        if let HNSWMutation::Replace { vectors, .. } = mutation {
            add(&mut additions, vectors.len())?;
            for vector in *vectors {
                control.check()?;
                validate_vector_values(dimensions, vector)?;
            }
        }
    }
    Ok(additions)
}

pub(super) fn candidate(
    index: &HNSWIndex,
    additions: usize,
    control: &StorageReadControl,
) -> StorageBackendResult<MemoryReservation> {
    let mut count = index.nodes.len();
    add(&mut count, additions)?;
    let mut memory = control.memory().reserve(node_workspace(index, count)?)?;
    let mut original_layers = 0;
    for node in index.nodes.values() {
        control.check()?;
        add(&mut original_layers, node.neighbors.len())?;
    }
    // Before compaction, insertions follow the old allocator; after compaction all IDs fit in 1..=count. Reserve the larger complete layer inventory, not MAX_HNSW_LEVEL for every node.
    for ordinal in 0..additions {
        control.check()?;
        let level = u64::try_from(ordinal)
            .ok()
            .and_then(|ordinal| index.next_node_id.checked_add(ordinal))
            .map_or(MAX_HNSW_LEVEL, |id| {
                deterministic_level(index.params.seed, id, index.params.m)
            });
        add(&mut original_layers, level + 1)?;
    }
    let mut rebuilt_layers = 0;
    for ordinal in 0..count {
        control.check()?;
        let id = u64::try_from(ordinal)
            .ok()
            .and_then(|ordinal| ordinal.checked_add(1))
            .ok_or(MemoryError::SizeOverflow)?;
        add(
            &mut rebuilt_layers,
            deterministic_level(index.params.seed, id, index.params.m) + 1,
        )?;
    }
    let layers = original_layers.max(rebuilt_layers);
    let degree = index.params.m.saturating_mul(2).min(count);
    let slots = degree.checked_add(1).ok_or(MemoryError::SizeOverflow)?;
    let mut per_layer = size_of::<Vec<NodeId>>();
    add(
        &mut per_layer,
        product(product(slots, 2)?, size_of::<NodeId>())?,
    )?;
    memory.grow(product(layers, per_layer)?)?;
    Ok(memory)
}

fn node_workspace(index: &HNSWIndex, count: usize) -> Result<usize, MemoryError> {
    let coordinates = product(index.dimensions as usize, size_of::<f32>())?;
    // Logical tree entries plus graph raw/normalized vectors, copied input, compaction vectors and a normalization temporary. Allocator node bookkeeping is separate, as for other storage owner reservations.
    let mut per_node = size_of::<(NodeId, HNSWNode)>()
        + size_of::<((DocId, u32), NodeId)>()
        + size_of::<NodeId>()
        + size_of::<(DocId, u32, Vec<f32>)>()
        + size_of::<Vec<f32>>();
    add(&mut per_node, product(coordinates, 5)?)?;
    // Search heaps, returned candidates and pruning candidates coexist. Include geometric vector/heap capacity, visited IDs, selected/rejected/removed neighbors and pruning sets.
    add(
        &mut per_node,
        8 * size_of::<Candidate>() + 16 * size_of::<NodeId>(),
    )?;
    product(count, per_node)
}

pub(super) fn snapshot(node: &HNSWNode) -> Result<usize, MemoryError> {
    let mut bytes = size_of::<HNSWNodeSnapshot>();
    add(
        &mut bytes,
        product(node.raw_vector.len(), size_of::<f32>())?,
    )?;
    add(
        &mut bytes,
        product(node.neighbors.len(), size_of::<Vec<NodeId>>())?,
    )?;
    for layer in &node.neighbors {
        add(&mut bytes, product(layer.len(), size_of::<NodeId>())?)?;
    }
    Ok(bytes)
}

pub(super) fn product(left: usize, right: usize) -> Result<usize, MemoryError> {
    left.checked_mul(right).ok_or(MemoryError::SizeOverflow)
}

pub(super) fn add(total: &mut usize, value: usize) -> Result<(), MemoryError> {
    *total = total.checked_add(value).ok_or(MemoryError::SizeOverflow)?;
    Ok(())
}
