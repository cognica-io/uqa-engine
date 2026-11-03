//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A private candidate owns every admitted entity, membership and reconstructed index.

use super::{
    invalid,
    retention::{self, entries, ordered},
    validation, Budgeted, Edge, GraphLabelRegistry, ImageInput, LabelKind, MemoryGraphStore,
    Partition, StorageBackendResult, StorageReadControl, Vertex, MAGIC,
};
use std::collections::BTreeMap;
use uqa_core::{memory::MemoryReservation, Value};

pub(super) fn read(
    reader: &mut dyn std::io::Read,
    control: &StorageReadControl,
) -> StorageBackendResult<Budgeted<MemoryGraphStore>> {
    control.check()?;
    let mut input = ImageInput { reader, control };
    if &input.array::<8>()? != MAGIC {
        return Err(invalid());
    }
    let mut memory = control.memory().reserve(size_of::<MemoryGraphStore>())?;
    let mut store = MemoryGraphStore {
        next_vertex_id: u64::from_le_bytes(input.array()?),
        next_edge_id: u64::from_le_bytes(input.array()?),
        ..MemoryGraphStore::default()
    };
    read_entities(&mut input, &mut store, &mut memory)?;
    let count = input.count()?;
    entries::<(String, Partition)>(&mut memory, count)?;
    for _ in 0..count {
        let name = text(&mut input, &mut memory)?;
        ordered(&store.graphs, &name)?;
        let mut partition = Partition::default();
        let vertices = input.count()?;
        entries::<u64>(&mut memory, vertices)?;
        for _ in 0..vertices {
            let id = u64::from_le_bytes(input.array()?);
            if partition.vertex_ids.last().is_some_and(|last| *last >= id) {
                return Err(invalid());
            }
            let vertex = store.vertices.get(&id).ok_or_else(invalid)?;
            retention::label_index(
                &mut partition.vertex_label_index,
                &vertex.label,
                id,
                &mut memory,
                control,
            )?;
            retention::membership(
                &mut store.vertex_membership,
                id,
                &name,
                &mut memory,
                control,
            )?;
            partition.vertex_ids.insert(id);
        }
        let edges = input.count()?;
        entries::<u64>(&mut memory, edges)?;
        for _ in 0..edges {
            let id = u64::from_le_bytes(input.array()?);
            if partition.edge_ids.last().is_some_and(|last| *last >= id) {
                return Err(invalid());
            }
            let edge = store.edges.get(&id).ok_or_else(invalid)?;
            retention::label_index(
                &mut partition.label_index,
                &edge.label,
                id,
                &mut memory,
                control,
            )?;
            retention::adjacency(&mut partition.adj_out, edge.source_id, id, &mut memory)?;
            retention::adjacency(&mut partition.adj_in, edge.target_id, id, &mut memory)?;
            retention::membership(&mut store.edge_membership, id, &name, &mut memory, control)?;
            partition.edge_ids.insert(id);
        }
        store.graphs.insert(name, partition);
    }
    let count = input.count()?;
    entries::<(String, GraphLabelRegistry)>(&mut memory, count)?;
    for _ in 0..count {
        let name = text(&mut input, &mut memory)?;
        ordered(&store.label_registries, &name)?;
        let registry = registry(&mut input, &mut memory)?;
        store.label_registries.insert(name, registry);
    }
    input.end()?;
    validation::validate(&store, control)?;
    Ok(Budgeted::new(store, memory))
}

fn read_entities(
    input: &mut ImageInput<'_>,
    store: &mut MemoryGraphStore,
    memory: &mut MemoryReservation,
) -> StorageBackendResult<()> {
    let count = input.count()?;
    entries::<(u64, Vertex)>(memory, count)?;
    for _ in 0..count {
        let vertex_id = u64::from_le_bytes(input.array()?);
        ordered(&store.vertices, &vertex_id)?;
        let label = text(input, memory)?;
        let properties = properties(input, memory)?;
        store.vertices.insert(
            vertex_id,
            Vertex {
                vertex_id,
                label,
                properties,
            },
        );
    }
    let count = input.count()?;
    entries::<(u64, Edge)>(memory, count)?;
    for _ in 0..count {
        let edge_id = u64::from_le_bytes(input.array()?);
        ordered(&store.edges, &edge_id)?;
        let source_id = u64::from_le_bytes(input.array()?);
        let target_id = u64::from_le_bytes(input.array()?);
        let label = text(input, memory)?;
        let properties = properties(input, memory)?;
        store.edges.insert(
            edge_id,
            Edge {
                edge_id,
                source_id,
                target_id,
                label,
                properties,
            },
        );
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

fn registry(
    input: &mut ImageInput<'_>,
    memory: &mut MemoryReservation,
) -> StorageBackendResult<GraphLabelRegistry> {
    let mut registry = GraphLabelRegistry {
        allocation_id: input.array()?,
        next_label_id: u32::from_le_bytes(input.array()?),
        ..GraphLabelRegistry::default()
    };
    let count = input.count()?;
    entries::<(String, u32)>(memory, count)?;
    for _ in 0..count {
        let label = text(input, memory)?;
        ordered(&registry.labels, &label)?;
        registry
            .labels
            .insert(label, u32::from_le_bytes(input.array()?));
    }
    let count = input.count()?;
    entries::<(String, LabelKind)>(memory, count)?;
    for _ in 0..count {
        let label = text(input, memory)?;
        ordered(&registry.kinds, &label)?;
        let kind = match input.array::<1>()?[0] {
            0 => LabelKind::Vertex,
            1 => LabelKind::Edge,
            _ => return Err(invalid()),
        };
        registry.kinds.insert(label, kind);
    }
    let count = input.count()?;
    entries::<(u32, u64)>(memory, count)?;
    for _ in 0..count {
        let id = u32::from_le_bytes(input.array()?);
        ordered(&registry.sequences, &id)?;
        registry
            .sequences
            .insert(id, u64::from_le_bytes(input.array()?));
    }
    let count = input.count()?;
    entries::<u32>(memory, count)?;
    for _ in 0..count {
        let id = u32::from_le_bytes(input.array()?);
        if registry
            .dropped_label_ids
            .last()
            .is_some_and(|last| *last >= id)
        {
            return Err(invalid());
        }
        registry.dropped_label_ids.insert(id);
    }
    Ok(registry)
}
