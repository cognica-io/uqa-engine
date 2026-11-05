//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Deterministic primary images borrow entity properties and ordered memberships.

use super::{
    ImageOutput, LabelKind, MemoryGraphStore, NativeLabels, StorageBackendResult,
    StorageReadControl, MAGIC,
};

pub(super) fn write(
    store: &MemoryGraphStore,
    writer: &mut dyn std::io::Write,
    control: &StorageReadControl,
) -> StorageBackendResult<u64> {
    let mut out = ImageOutput {
        writer,
        control,
        written: 0,
    };
    out.bytes(MAGIC)?;
    out.bytes(&store.next_vertex_id.to_le_bytes())?;
    out.bytes(&store.next_edge_id.to_le_bytes())?;
    out.count(store.vertices.len())?;
    for (id, vertex) in &store.vertices {
        out.bytes(&id.to_le_bytes())?;
        out.sized(vertex.label.as_bytes())?;
        out.fields(
            vertex
                .properties
                .iter()
                .map(|(key, value)| (key.as_str(), value)),
        )?;
    }
    out.count(store.edges.len())?;
    for (id, edge) in &store.edges {
        for id in [*id, edge.source_id, edge.target_id] {
            out.bytes(&id.to_le_bytes())?;
        }
        out.sized(edge.label.as_bytes())?;
        out.fields(
            edge.properties
                .iter()
                .map(|(key, value)| (key.as_str(), value)),
        )?;
    }
    out.count(store.graphs.len())?;
    for (name, graph) in &store.graphs {
        out.sized(name.as_bytes())?;
        for ids in [&graph.vertex_ids, &graph.edge_ids] {
            out.count(ids.len())?;
            for id in ids {
                out.bytes(&id.to_le_bytes())?;
            }
        }
    }
    out.count(store.label_registries.len())?;
    for (name, registry) in &store.label_registries {
        out.sized(name.as_bytes())?;
        write_registry(&mut out, registry)?;
    }
    control.check()?;
    Ok(out.written)
}

fn write_registry(out: &mut ImageOutput<'_>, registry: &NativeLabels) -> StorageBackendResult<()> {
    out.bytes(&registry.allocation_id)?;
    out.bytes(&registry.next_label_id.to_le_bytes())?;
    out.count(registry.labels.len())?;
    for (name, id) in &registry.labels {
        out.sized(name.as_bytes())?;
        out.bytes(&id.to_le_bytes())?;
    }
    out.count(registry.kinds.len())?;
    for (name, kind) in &registry.kinds {
        out.sized(name.as_bytes())?;
        out.bytes(&[match kind {
            LabelKind::Vertex => 0,
            LabelKind::Edge => 1,
        }])?;
    }
    out.count(registry.sequences.len())?;
    for (id, sequence) in &registry.sequences {
        out.bytes(&id.to_le_bytes())?;
        out.bytes(&sequence.to_le_bytes())?;
    }
    out.count(registry.dropped_label_ids.len())?;
    for id in &registry.dropped_label_ids {
        out.bytes(&id.to_le_bytes())?;
    }
    Ok(())
}
