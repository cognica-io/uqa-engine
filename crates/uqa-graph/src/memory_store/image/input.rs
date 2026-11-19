//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Decoding transfers each admitted payload directly into its native immutable owner.

use super::super::publication::{entity_bytes_with, membership_with};
use super::{
    invalid,
    retention::{ordered, ordered_text},
    validation, Edge, LabelKind, MemoryGraphStore, NativeLabels, Partition, StorageBackendError,
    StorageBackendResult, StorageReadControl, Vertex, MAGIC,
};
use std::collections::BTreeMap;
use uqa_core::{
    image_io::AsyncImageInput,
    memory::{MemoryError, MemoryReservation},
    Value,
};

pub(super) async fn read_from(
    input: &mut AsyncImageInput<'_>,
) -> StorageBackendResult<MemoryGraphStore> {
    let control = StorageReadControl::new(input.memory, input.cancellation);
    if &input.array::<8>().await? != MAGIC {
        return Err(invalid());
    }
    let mut store = MemoryGraphStore::with_memory(control.memory());
    store.next_vertex_id = u64::from_le_bytes(input.array().await?);
    store.next_edge_id = u64::from_le_bytes(input.array().await?);
    read_entities(input, &mut store).await?;
    let count = item_count::<(String, Partition)>(input).await?;
    for _ in 0..count {
        let mut name_memory = control.memory().empty_reservation();
        let name = text(input, &mut name_memory).await?;
        ordered_text(
            store.graphs.last_key_value().map(|(key, _)| key),
            &name,
            input,
        )
        .await?;
        let mut partition = Partition::new(control.memory());
        let vertices = item_count::<u64>(input).await?;
        for _ in 0..vertices {
            let id = u64::from_le_bytes(input.array().await?);
            if partition.vertex_ids.last().is_some_and(|last| *last >= id) {
                return Err(invalid());
            }
            let vertex = store.vertices.get(&id).ok_or_else(invalid)?;
            partition
                .add_vertex_with::<StorageBackendError>(vertex, input)
                .await?;
            membership_with::<StorageBackendError>(
                &mut store.vertex_membership,
                id,
                &name,
                true,
                input,
            )
            .await?;
        }
        let edges = item_count::<u64>(input).await?;
        for _ in 0..edges {
            let id = u64::from_le_bytes(input.array().await?);
            if partition.edge_ids.last().is_some_and(|last| *last >= id) {
                return Err(invalid());
            }
            let edge = store.edges.get(&id).ok_or_else(invalid)?;
            partition
                .add_edge_with::<StorageBackendError>(edge, input)
                .await?;
            membership_with::<StorageBackendError>(
                &mut store.edge_membership,
                id,
                &name,
                true,
                input,
            )
            .await?;
        }
        let bytes = name
            .capacity()
            .checked_add(partition.retained_bytes()?)
            .ok_or(MemoryError::SizeOverflow)?;
        store
            .graphs
            .insert_admitted(name, partition, name_memory, bytes)?;
    }
    let count = item_count::<(String, NativeLabels)>(input).await?;
    for _ in 0..count {
        let mut payload = control.memory().empty_reservation();
        let name = text(input, &mut payload).await?;
        ordered_text(
            store.label_registries.last_key_value().map(|(key, _)| key),
            &name,
            input,
        )
        .await?;
        let registry = registry(input).await?;
        let bytes = name
            .capacity()
            .checked_add(registry.retained_bytes()?)
            .ok_or(MemoryError::SizeOverflow)?;
        store
            .label_registries
            .insert_admitted(name, registry, payload, bytes)?;
    }
    input.end().await?;
    {
        let mut validation = validation::Validation::new(&store);
        loop {
            input.checkpoint().await?;
            if validation.advance(1, &control)?.ready {
                break;
            }
        }
    }
    Ok(store)
}

async fn read_entities(
    input: &mut AsyncImageInput<'_>,
    store: &mut MemoryGraphStore,
) -> StorageBackendResult<()> {
    let count = item_count::<(u64, Vertex)>(input).await?;
    for _ in 0..count {
        let vertex_id = u64::from_le_bytes(input.array().await?);
        ordered(
            store.vertices.last_key_value().map(|(key, _)| key),
            &vertex_id,
        )?;
        let mut memory = input.memory.empty_reservation();
        let label = text(input, &mut memory).await?;
        let properties = properties(input, &mut memory).await?;
        let bytes = entity_bytes_with(&label, &properties, input.memory, input).await?;
        store.vertices.insert_admitted(
            vertex_id,
            Vertex {
                vertex_id,
                label,
                properties,
            },
            memory,
            bytes,
        )?;
    }
    let count = item_count::<(u64, Edge)>(input).await?;
    for _ in 0..count {
        let edge_id = u64::from_le_bytes(input.array().await?);
        ordered(store.edges.last_key_value().map(|(key, _)| key), &edge_id)?;
        let source_id = u64::from_le_bytes(input.array().await?);
        let target_id = u64::from_le_bytes(input.array().await?);
        let mut memory = input.memory.empty_reservation();
        let label = text(input, &mut memory).await?;
        let properties = properties(input, &mut memory).await?;
        let bytes = entity_bytes_with(&label, &properties, input.memory, input).await?;
        store.edges.insert_admitted(
            edge_id,
            Edge {
                edge_id,
                source_id,
                target_id,
                label,
                properties,
            },
            memory,
            bytes,
        )?;
    }
    Ok(())
}

async fn text(
    input: &mut AsyncImageInput<'_>,
    memory: &mut MemoryReservation,
) -> StorageBackendResult<String> {
    let (text, lease) = input.text().await?.into_parts();
    memory.absorb(lease);
    Ok(text)
}

async fn properties(
    input: &mut AsyncImageInput<'_>,
    memory: &mut MemoryReservation,
) -> StorageBackendResult<BTreeMap<String, Value>> {
    let (value, lease) = uqa_storage::image_io::read_value_image_from(input)
        .await?
        .into_parts();
    memory.absorb(lease);
    let Value::Map(fields) = value else {
        return Err(invalid());
    };
    Ok(fields)
}

async fn registry(input: &mut AsyncImageInput<'_>) -> StorageBackendResult<NativeLabels> {
    let mut registry = NativeLabels::new(input.memory);
    registry.allocation_id = input.array().await?;
    registry.next_label_id = u32::from_le_bytes(input.array().await?);
    let count = item_count::<(String, u32)>(input).await?;
    for _ in 0..count {
        let label = input.text().await?;
        ordered_text(
            registry.labels.last_key_value().map(|(key, _)| key),
            &label,
            input,
        )
        .await?;
        let id = u32::from_le_bytes(input.array().await?);
        let bytes = label.capacity();
        let (label, memory) = label.into_parts();
        registry.labels.insert_admitted(label, id, memory, bytes)?;
    }
    let count = item_count::<(String, LabelKind)>(input).await?;
    for _ in 0..count {
        let label = input.text().await?;
        ordered_text(
            registry.kinds.last_key_value().map(|(key, _)| key),
            &label,
            input,
        )
        .await?;
        let kind = match input.array::<1>().await?[0] {
            0 => LabelKind::Vertex,
            1 => LabelKind::Edge,
            _ => return Err(invalid()),
        };
        let bytes = label.capacity();
        let (label, memory) = label.into_parts();
        registry.kinds.insert_admitted(label, kind, memory, bytes)?;
    }
    let count = item_count::<(u32, u64)>(input).await?;
    for _ in 0..count {
        let id = u32::from_le_bytes(input.array().await?);
        ordered(registry.sequences.last_key_value().map(|(key, _)| key), &id)?;
        registry
            .sequences
            .insert(id, u64::from_le_bytes(input.array().await?), 0, 0)?;
    }
    let count = item_count::<u32>(input).await?;
    for _ in 0..count {
        let id = u32::from_le_bytes(input.array().await?);
        if registry
            .dropped_label_ids
            .last()
            .is_some_and(|last| *last >= id)
        {
            return Err(invalid());
        }
        registry.dropped_label_ids.insert(id, 0)?;
    }
    Ok(registry)
}

async fn item_count<T>(input: &mut AsyncImageInput<'_>) -> StorageBackendResult<usize> {
    let count = input.count().await?;
    count
        .checked_mul(size_of::<T>())
        .ok_or(MemoryError::SizeOverflow)?;
    Ok(count)
}
