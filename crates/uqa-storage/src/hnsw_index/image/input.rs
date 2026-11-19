//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Fresh ordered builders retain exact evaluated vectors and adjacency under the import allowance.

use super::{
    active_key, flag, invalid, read_floats, validation, Budgeted, BudgetedVec, HNSWIndex, HNSWNode,
    HNSWVector, Input, StorageBackendResult, MAX_HNSW_LEVEL,
};
use crate::{spill_map::Builder, HNSWIndexParams};

pub(crate) async fn read(
    input: &mut Input<'_, '_>,
    dimensions: u32,
) -> StorageBackendResult<HNSWIndex> {
    let mut memory = input.control.memory().reserve(size_of::<HNSWIndex>())?;
    let params = HNSWIndexParams {
        m: input.source.count().await?,
        ef_construction: input.source.count().await?,
        ef_search: input.source.count().await?,
        rebuild_threshold: input.source.count().await?,
        seed: u64::from_le_bytes(input.source.array().await?),
    }
    .validate()?;
    let mut index = HNSWIndex::with_memory(dimensions, params, input.control.memory())?;
    index.entry_point = if flag(input).await? {
        Some(u64::from_le_bytes(input.source.array().await?))
    } else {
        None
    };
    index.max_level = input.source.count().await?;
    if index.max_level > MAX_HNSW_LEVEL {
        return Err(invalid());
    }
    index.next_node_id = u64::from_le_bytes(input.source.array().await?);
    index.deleted_count = input.source.count().await?;
    index.full_rewrite = flag(input).await?;
    read_nodes(input, &mut index).await?;
    let count = input.source.count().await?;
    let mut previous = None;
    for _ in 0..count {
        let id = u64::from_le_bytes(input.source.array().await?);
        if previous.is_some_and(|previous| previous >= id) {
            return Err(invalid());
        }
        let value = u64::from_le_bytes(input.source.array().await?);
        index
            .dirty_nodes
            .insert_from(u128::from(id), value, Some(input.control), input.source)
            .await?;
        previous = Some(id);
    }
    validation::validate_from(&index, input.control, input.source).await?;
    memory.grow(size_of::<uqa_core::memory::MemoryReservation>())?;
    index.image_memory = Some(std::sync::Arc::new(memory));
    Ok(
        index.into_mutation_budget(&uqa_core::memory::MemoryBudget::new(
            crate::mvcc::VersionedSessionOptions::default().retained_bytes,
        )),
    )
}

async fn read_nodes(input: &mut Input<'_, '_>, index: &mut HNSWIndex) -> StorageBackendResult<()> {
    let memory = input.control.memory();
    let mut nodes = Builder::new(memory, memory.limit() / 16);
    let mut raw_vectors = Builder::new(memory, memory.limit() / 32);
    let mut normalized_vectors = Builder::new(memory, memory.limit() / 32);
    let count = input.source.count().await?;
    let mut previous = None;
    for _ in 0..count {
        let id = u64::from_le_bytes(input.source.array().await?);
        if previous.is_some_and(|previous| previous >= id) {
            return Err(invalid());
        }
        let document = u64::from_le_bytes(input.source.array().await?);
        let ordinal = u32::from_le_bytes(input.source.array().await?);
        let level = input.source.count().await?;
        if level > MAX_HNSW_LEVEL {
            return Err(invalid());
        }
        let deleted = flag(input).await?;
        for builder in [&mut raw_vectors, &mut normalized_vectors] {
            let norm = f32::from_bits(u32::from_le_bytes(input.source.array().await?));
            let vector = read_floats(input, index.dimensions).await?;
            let (values, lease) = vector.into_parts();
            builder
                .insert_from(
                    u128::from(id),
                    HNSWVector { values, norm },
                    Some(input.control),
                    input.source,
                )
                .await?;
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
        )
        .await?;
        if !deleted {
            let key = active_key(document, ordinal);
            if index.active.contains_key(key)? {
                return Err(invalid());
            }
            index
                .active
                .insert_from(key, id, Some(input.control), input.source)
                .await?;
        }
        let (node, lease) = node.into_parts();
        nodes
            .insert_from(u128::from(id), node, Some(input.control), input.source)
            .await?;
        drop(lease);
        previous = Some(id);
    }
    index.nodes = nodes.finish(Some(input.control))?;
    index.raw_vectors = raw_vectors.finish(Some(input.control))?;
    index.normalized_vectors = normalized_vectors.finish(Some(input.control))?;
    Ok(())
}

async fn read_node_layers(
    input: &mut Input<'_, '_>,
    index: &HNSWIndex,
    mut node: HNSWNode,
) -> StorageBackendResult<Budgeted<HNSWNode>> {
    let mut memory = input.control.memory().reserve(size_of::<HNSWNode>())?;
    let mut layers = BudgetedVec::new(input.control.memory());
    layers.reserve(node.level + 1)?;
    for layer in 0..=node.level {
        let count = input.source.count().await?;
        if count > index.max_connections(layer) {
            return Err(invalid());
        }
        let mut neighbors = BudgetedVec::new(input.control.memory());
        neighbors.reserve(count)?;
        for _ in 0..count {
            neighbors.push(u64::from_le_bytes(input.source.array().await?))?;
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
