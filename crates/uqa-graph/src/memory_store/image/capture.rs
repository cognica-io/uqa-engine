//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Admission covers the immutable source's primary payloads and existing derived indexes.

use super::{
    retention::entries, GraphLabelRegistry, MemoryGraphStore, Partition, StorageBackendResult,
    StorageReadControl,
};
use std::collections::{BTreeMap, BTreeSet};
use uqa_core::{memory::MemoryReservation, Edge, Value, Vertex};

impl MemoryGraphStore {
    pub(crate) fn image_retention(
        &self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<MemoryReservation> {
        control.check()?;
        let Self {
            vertices,
            edges,
            graphs,
            vertex_membership,
            edge_membership,
            label_registries,
            next_vertex_id: _,
            next_edge_id: _,
        } = self;
        let mut memory = control.memory().reserve(size_of::<Self>())?;
        entries::<(u64, Vertex)>(&mut memory, vertices.len())?;
        for Vertex {
            vertex_id: _,
            label,
            properties,
        } in vertices.values()
        {
            entity(label, properties, &mut memory, control)?;
        }
        entries::<(u64, Edge)>(&mut memory, edges.len())?;
        for Edge {
            edge_id: _,
            source_id: _,
            target_id: _,
            label,
            properties,
        } in edges.values()
        {
            entity(label, properties, &mut memory, control)?;
        }
        entries::<(String, Partition)>(&mut memory, graphs.len())?;
        for (name, partition) in graphs {
            control.check()?;
            memory.grow(name.capacity())?;
            let Partition {
                vertex_ids,
                edge_ids,
                adj_out,
                adj_in,
                label_index,
                vertex_label_index,
            } = partition;
            entries::<u64>(&mut memory, vertex_ids.len())?;
            entries::<u64>(&mut memory, edge_ids.len())?;
            for adjacency in [adj_out, adj_in] {
                entries::<(u64, BTreeSet<u64>)>(&mut memory, adjacency.len())?;
                for neighbors in adjacency.values() {
                    control.check()?;
                    entries::<u64>(&mut memory, neighbors.len())?;
                }
            }
            for labels in [label_index, vertex_label_index] {
                entries::<(String, BTreeSet<u64>)>(&mut memory, labels.len())?;
                for (label, ids) in labels {
                    control.check()?;
                    memory.grow(label.capacity())?;
                    entries::<u64>(&mut memory, ids.len())?;
                }
            }
        }
        for membership in [vertex_membership, edge_membership] {
            entries::<(u64, BTreeSet<String>)>(&mut memory, membership.len())?;
            for names in membership.values() {
                entries::<String>(&mut memory, names.len())?;
                for name in names {
                    control.check()?;
                    memory.grow(name.capacity())?;
                }
            }
        }
        entries::<(String, GraphLabelRegistry)>(&mut memory, label_registries.len())?;
        for (name, registry) in label_registries {
            control.check()?;
            memory.grow(name.capacity())?;
            let GraphLabelRegistry {
                allocation_id: _,
                labels,
                kinds,
                sequences,
                dropped_label_ids,
                next_label_id: _,
            } = registry;
            entries::<(String, u32)>(&mut memory, labels.len())?;
            entries::<(String, super::LabelKind)>(&mut memory, kinds.len())?;
            for name in labels.keys().chain(kinds.keys()) {
                control.check()?;
                memory.grow(name.capacity())?;
            }
            entries::<(u32, u64)>(&mut memory, sequences.len())?;
            entries::<u32>(&mut memory, dropped_label_ids.len())?;
        }
        control.check()?;
        Ok(memory)
    }
}

fn entity(
    label: &String,
    properties: &BTreeMap<String, Value>,
    memory: &mut MemoryReservation,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    control.check()?;
    memory.grow(label.capacity())?;
    entries::<(String, Value)>(memory, properties.len())?;
    for (name, value) in properties {
        control.check()?;
        memory.grow(name.capacity())?;
        let bytes = value.retained_payload_bytes_with_check(control.memory(), || {
            control.cancellation().check()?;
            Ok(())
        })?;
        memory.grow(bytes)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::GraphStore;

    #[test]
    fn retained_graph_payload_admission_counts_unused_source_capacity() {
        let mut source = MemoryGraphStore::default();
        source.create_graph("g");
        source.add_vertex(Vertex::new(1, "label"), "g").unwrap();
        let control = StorageReadControl::with_limit(2 << 20);
        let initial = source.image_retention(&control).unwrap();
        let initial_bytes = initial.bytes();
        drop(initial);
        let mut value = String::with_capacity(1 << 20);
        value.push('x');
        let capacity = value.capacity();
        source
            .vertices
            .get_mut(&1)
            .unwrap()
            .properties
            .insert("payload".into(), Value::Str(value));
        let retained = source.image_retention(&control).unwrap();
        assert!(retained.bytes() >= initial_bytes + capacity);
        drop(retained);
        assert_eq!(control.memory().used(), 0);
        let small = StorageReadControl::with_limit(4096);
        assert!(matches!(
            source.image_retention(&small),
            Err(uqa_storage::StorageBackendError::Memory(_))
        ));
        assert_eq!(small.memory().used(), 0);
    }
}
