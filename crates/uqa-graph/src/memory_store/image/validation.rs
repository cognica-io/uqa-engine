//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Validate primary/derived ownership without repairing source corruption or collecting another graph.

use super::{invalid, MemoryGraphStore, Partition, StorageBackendResult, StorageReadControl};

pub(super) fn validate(
    store: &MemoryGraphStore,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    control.check()?;
    if store
        .vertices
        .last_key_value()
        .is_some_and(|(id, _)| *id >= store.next_vertex_id)
        || store
            .edges
            .last_key_value()
            .is_some_and(|(id, _)| *id >= store.next_edge_id)
    {
        return Err(invalid());
    }
    for (id, vertex) in &store.vertices {
        control.check()?;
        if *id != vertex.vertex_id {
            return Err(invalid());
        }
    }
    for (id, edge) in &store.edges {
        control.check()?;
        if *id != edge.edge_id {
            return Err(invalid());
        }
    }
    for (name, part) in &store.graphs {
        control.check()?;
        validate_partition(store, name, part, control)?;
    }
    for (id, graphs) in &store.vertex_membership {
        control.check()?;
        for graph in graphs {
            control.check()?;
            if !store.vertices.contains_key(id)
                || !store
                    .graphs
                    .get(graph)
                    .is_some_and(|p| p.vertex_ids.contains(id))
            {
                return Err(invalid());
            }
        }
    }
    for (id, graphs) in &store.edge_membership {
        control.check()?;
        for graph in graphs {
            control.check()?;
            if !store.edges.contains_key(id)
                || !store
                    .graphs
                    .get(graph)
                    .is_some_and(|p| p.edge_ids.contains(id))
            {
                return Err(invalid());
            }
        }
    }
    control.check()
}

fn validate_partition(
    store: &MemoryGraphStore,
    name: &str,
    part: &Partition,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    for id in &part.vertex_ids {
        control.check()?;
        let vertex = store.vertices.get(id).ok_or_else(invalid)?;
        if !store
            .vertex_membership
            .get(id)
            .is_some_and(|names| names.contains(name))
            || !part
                .vertex_label_index
                .get(&vertex.label)
                .is_some_and(|ids| ids.contains(id))
        {
            return Err(invalid());
        }
    }
    for id in &part.edge_ids {
        control.check()?;
        let edge = store.edges.get(id).ok_or_else(invalid)?;
        if !store
            .edge_membership
            .get(id)
            .is_some_and(|names| names.contains(name))
            || !part
                .label_index
                .get(&edge.label)
                .is_some_and(|ids| ids.contains(id))
            || !part
                .adj_out
                .get(&edge.source_id)
                .is_some_and(|ids| ids.contains(id))
            || !part
                .adj_in
                .get(&edge.target_id)
                .is_some_and(|ids| ids.contains(id))
        {
            return Err(invalid());
        }
    }
    for (label, ids) in &part.vertex_label_index {
        control.check()?;
        for id in ids {
            control.check()?;
            if !part.vertex_ids.contains(id)
                || store.vertices.get(id).is_none_or(|v| &v.label != label)
            {
                return Err(invalid());
            }
        }
    }
    for (label, ids) in &part.label_index {
        control.check()?;
        for id in ids {
            control.check()?;
            if !part.edge_ids.contains(id) || store.edges.get(id).is_none_or(|e| &e.label != label)
            {
                return Err(invalid());
            }
        }
    }
    for (outgoing, index) in [(true, &part.adj_out), (false, &part.adj_in)] {
        for (vertex, ids) in index {
            control.check()?;
            for id in ids {
                control.check()?;
                let edge = store.edges.get(id).ok_or_else(invalid)?;
                if !part.edge_ids.contains(id)
                    || *vertex
                        != if outgoing {
                            edge.source_id
                        } else {
                            edge.target_id
                        }
                {
                    return Err(invalid());
                }
            }
        }
    }
    Ok(())
}
