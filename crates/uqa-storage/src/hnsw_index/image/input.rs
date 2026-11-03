//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Fresh ordered builders retain exact evaluated vectors and adjacency under the import allowance.

use super::{
    active_key, flag, invalid, read_floats, validation, Budgeted, BudgetedVec, HNSWIndex, HNSWNode,
    HNSWVector, ImageInput, StorageBackendResult, MAX_HNSW_LEVEL,
};
use crate::{spill_map::Builder, HNSWIndexParams};

pub(crate) fn read(
    input: &mut ImageInput<'_>,
    dimensions: u32,
) -> StorageBackendResult<Budgeted<HNSWIndex>> {
    let memory = input.control.memory().reserve(size_of::<HNSWIndex>())?;
    let params = HNSWIndexParams {
        m: input.count()?,
        ef_construction: input.count()?,
        ef_search: input.count()?,
        rebuild_threshold: input.count()?,
        seed: u64::from_le_bytes(input.array()?),
    }
    .validate()?;
    let mut index = HNSWIndex::with_memory(dimensions, params, input.control.memory())?;
    index.entry_point = if flag(input)? {
        Some(u64::from_le_bytes(input.array()?))
    } else {
        None
    };
    index.max_level = input.count()?;
    if index.max_level > MAX_HNSW_LEVEL {
        return Err(invalid());
    }
    index.next_node_id = u64::from_le_bytes(input.array()?);
    index.deleted_count = input.count()?;
    index.full_rewrite = flag(input)?;
    read_nodes(input, &mut index)?;
    let count = input.count()?;
    let mut previous = None;
    for _ in 0..count {
        let id = u64::from_le_bytes(input.array()?);
        if previous.is_some_and(|previous| previous >= id) {
            return Err(invalid());
        }
        let value = u64::from_le_bytes(input.array()?);
        index
            .dirty_nodes
            .insert(u128::from(id), value, Some(input.control))?;
        previous = Some(id);
    }
    validation::validate(&index, input.control)?;
    Ok(Budgeted::new(index, memory))
}

fn read_nodes(input: &mut ImageInput<'_>, index: &mut HNSWIndex) -> StorageBackendResult<()> {
    let memory = input.control.memory();
    let mut nodes = Builder::new(memory, memory.limit() / 16);
    let mut raw_vectors = Builder::new(memory, memory.limit() / 32);
    let mut normalized_vectors = Builder::new(memory, memory.limit() / 32);
    let count = input.count()?;
    let mut previous = None;
    for _ in 0..count {
        let id = u64::from_le_bytes(input.array()?);
        if previous.is_some_and(|previous| previous >= id) {
            return Err(invalid());
        }
        let document = u64::from_le_bytes(input.array()?);
        let ordinal = u32::from_le_bytes(input.array()?);
        let level = input.count()?;
        if level > MAX_HNSW_LEVEL {
            return Err(invalid());
        }
        let deleted = flag(input)?;
        for builder in [&mut raw_vectors, &mut normalized_vectors] {
            let norm = f32::from_bits(u32::from_le_bytes(input.array()?));
            let vector = read_floats(input, index.dimensions)?;
            let (values, lease) = vector.into_parts();
            builder.insert(
                u128::from(id),
                HNSWVector { values, norm },
                Some(input.control),
            )?;
            drop(lease);
        }
        let node = read_node_layers(
            input,
            index,
            HNSWNode {
                id,
                doc_id: document,
                vector_ordinal: ordinal,
                level,
                deleted,
                neighbors: Vec::new(),
            },
        )?;
        if !deleted {
            let key = active_key(document, ordinal);
            if index.active.contains_key(key)? {
                return Err(invalid());
            }
            index.active.insert(key, id, Some(input.control))?;
        }
        let (node, lease) = node.into_parts();
        nodes.insert(u128::from(id), node, Some(input.control))?;
        drop(lease);
        previous = Some(id);
    }
    index.nodes = nodes.finish(Some(input.control))?;
    index.raw_vectors = raw_vectors.finish(Some(input.control))?;
    index.normalized_vectors = normalized_vectors.finish(Some(input.control))?;
    Ok(())
}

fn read_node_layers(
    input: &mut ImageInput<'_>,
    index: &HNSWIndex,
    mut node: HNSWNode,
) -> StorageBackendResult<Budgeted<HNSWNode>> {
    let mut memory = input.control.memory().reserve(size_of::<HNSWNode>())?;
    let mut layers = BudgetedVec::new(input.control.memory());
    layers.reserve(node.level + 1)?;
    for layer in 0..=node.level {
        let count = input.count()?;
        if count > index.max_connections(layer) {
            return Err(invalid());
        }
        let mut neighbors = BudgetedVec::new(input.control.memory());
        neighbors.reserve(count)?;
        for _ in 0..count {
            neighbors.push(u64::from_le_bytes(input.array()?))?;
        }
        let (neighbors, lease) = neighbors.into_parts();
        memory.absorb(lease);
        layers.push(neighbors)?;
    }
    let (layers, lease) = layers.into_parts();
    memory.absorb(lease);
    node.neighbors = layers;
    Ok(Budgeted::new(node, memory))
}
