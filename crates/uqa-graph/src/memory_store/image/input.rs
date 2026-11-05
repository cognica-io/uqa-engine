//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Decoding transfers each admitted payload directly into its native immutable owner.

use super::super::publication::{entity_bytes_with_check, membership};
use super::{
    invalid, retention::ordered, validation, Edge, ImageInput, LabelKind, MemoryGraphStore,
    NativeLabels, Partition, StorageBackendResult, StorageReadControl, Vertex, MAGIC,
};
use std::collections::BTreeMap;
use uqa_core::{
    memory::{MemoryError, MemoryReservation},
    Value,
};

pub(super) fn read(
    reader: &mut dyn std::io::Read,
    control: &StorageReadControl,
) -> StorageBackendResult<MemoryGraphStore> {
    control.check()?;
    let mut input = ImageInput { reader, control };
    if &input.array::<8>()? != MAGIC {
        return Err(invalid());
    }
    let mut store = MemoryGraphStore::with_memory(control.memory());
    store.next_vertex_id = u64::from_le_bytes(input.array()?);
    store.next_edge_id = u64::from_le_bytes(input.array()?);
    read_entities(&mut input, &mut store)?;
    let count = item_count::<(String, Partition)>(&mut input)?;
    for _ in 0..count {
        let mut name_memory = control.memory().empty_reservation();
        let name = text(&mut input, &mut name_memory)?;
        ordered(store.graphs.last_key_value().map(|(key, _)| key), &name)?;
        let mut partition = Partition::new(control.memory());
        let vertices = item_count::<u64>(&mut input)?;
        for _ in 0..vertices {
            let id = u64::from_le_bytes(input.array()?);
            if partition.vertex_ids.last().is_some_and(|last| *last >= id) {
                return Err(invalid());
            }
            let vertex = store.vertices.get(&id).ok_or_else(invalid)?;
            partition.add_vertex(vertex)?;
            membership(&mut store.vertex_membership, id, &name, true)?;
        }
        let edges = item_count::<u64>(&mut input)?;
        for _ in 0..edges {
            let id = u64::from_le_bytes(input.array()?);
            if partition.edge_ids.last().is_some_and(|last| *last >= id) {
                return Err(invalid());
            }
            let edge = store.edges.get(&id).ok_or_else(invalid)?;
            partition.add_edge(edge)?;
            membership(&mut store.edge_membership, id, &name, true)?;
        }
        let bytes = name
            .capacity()
            .checked_add(partition.retained_bytes()?)
            .ok_or(MemoryError::SizeOverflow)?;
        store
            .graphs
            .insert_admitted(name, partition, name_memory, bytes)?;
    }
    let count = item_count::<(String, NativeLabels)>(&mut input)?;
    for _ in 0..count {
        let mut payload = control.memory().empty_reservation();
        let name = text(&mut input, &mut payload)?;
        ordered(
            store.label_registries.last_key_value().map(|(key, _)| key),
            &name,
        )?;
        let registry = registry(&mut input)?;
        let bytes = name
            .capacity()
            .checked_add(registry.retained_bytes()?)
            .ok_or(MemoryError::SizeOverflow)?;
        store
            .label_registries
            .insert_admitted(name, registry, payload, bytes)?;
    }
    input.end()?;
    validation::validate(&store, control)?;
    Ok(store)
}

fn read_entities(
    input: &mut ImageInput<'_>,
    store: &mut MemoryGraphStore,
) -> StorageBackendResult<()> {
    let count = item_count::<(u64, Vertex)>(input)?;
    for _ in 0..count {
        let vertex_id = u64::from_le_bytes(input.array()?);
        ordered(
            store.vertices.last_key_value().map(|(key, _)| key),
            &vertex_id,
        )?;
        let mut memory = input.control.memory().empty_reservation();
        let label = text(input, &mut memory)?;
        let properties = properties(input, &mut memory)?;
        let bytes = entity_bytes_with_check(&label, &properties, input.control.memory(), || {
            input.control.cancellation().check()
        })?;
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
    let count = item_count::<(u64, Edge)>(input)?;
    for _ in 0..count {
        let edge_id = u64::from_le_bytes(input.array()?);
        ordered(store.edges.last_key_value().map(|(key, _)| key), &edge_id)?;
        let source_id = u64::from_le_bytes(input.array()?);
        let target_id = u64::from_le_bytes(input.array()?);
        let mut memory = input.control.memory().empty_reservation();
        let label = text(input, &mut memory)?;
        let properties = properties(input, &mut memory)?;
        let bytes = entity_bytes_with_check(&label, &properties, input.control.memory(), || {
            input.control.cancellation().check()
        })?;
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

fn text(
    input: &mut ImageInput<'_>,
    memory: &mut MemoryReservation,
) -> StorageBackendResult<String> {
    let (text, lease) = input.text()?.into_parts();
    memory.absorb(lease);
    Ok(text)
}

fn properties(
    input: &mut ImageInput<'_>,
    memory: &mut MemoryReservation,
) -> StorageBackendResult<BTreeMap<String, Value>> {
    let (value, lease) = input.value()?.into_parts();
    memory.absorb(lease);
    let Value::Map(fields) = value else {
        return Err(invalid());
    };
    Ok(fields)
}

fn registry(input: &mut ImageInput<'_>) -> StorageBackendResult<NativeLabels> {
    let mut registry = NativeLabels::new(input.control.memory());
    registry.allocation_id = input.array()?;
    registry.next_label_id = u32::from_le_bytes(input.array()?);
    let count = item_count::<(String, u32)>(input)?;
    for _ in 0..count {
        let (label, memory) = input.text()?.into_parts();
        ordered(registry.labels.last_key_value().map(|(key, _)| key), &label)?;
        let bytes = label.capacity();
        registry.labels.insert_admitted(
            label,
            u32::from_le_bytes(input.array()?),
            memory,
            bytes,
        )?;
    }
    let count = item_count::<(String, LabelKind)>(input)?;
    for _ in 0..count {
        let (label, memory) = input.text()?.into_parts();
        ordered(registry.kinds.last_key_value().map(|(key, _)| key), &label)?;
        let kind = match input.array::<1>()?[0] {
            0 => LabelKind::Vertex,
            1 => LabelKind::Edge,
            _ => return Err(invalid()),
        };
        let bytes = label.capacity();
        registry.kinds.insert_admitted(label, kind, memory, bytes)?;
    }
    let count = item_count::<(u32, u64)>(input)?;
    for _ in 0..count {
        let id = u32::from_le_bytes(input.array()?);
        ordered(registry.sequences.last_key_value().map(|(key, _)| key), &id)?;
        registry
            .sequences
            .insert(id, u64::from_le_bytes(input.array()?), 0, 0)?;
    }
    let count = item_count::<u32>(input)?;
    for _ in 0..count {
        let id = u32::from_le_bytes(input.array()?);
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

fn item_count<T>(input: &mut ImageInput<'_>) -> StorageBackendResult<usize> {
    let count = input.count()?;
    count
        .checked_mul(size_of::<T>())
        .ok_or(MemoryError::SizeOverflow)?;
    Ok(count)
}
