//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable per-graph membership, adjacency and label roots.

use super::{Edge, EdgeId, NativeMap, NativeSet, Vertex, VertexId};
use uqa_core::memory::{MemoryBudget, MemoryError};

#[derive(Debug, Clone)]
pub(super) struct Partition {
    pub(super) vertex_ids: NativeSet<VertexId>,
    pub(super) edge_ids: NativeSet<EdgeId>,
    pub(super) adj_out: NativeMap<VertexId, NativeSet<EdgeId>>,
    pub(super) adj_in: NativeMap<VertexId, NativeSet<EdgeId>>,
    pub(super) label_index: NativeMap<String, NativeSet<EdgeId>>,
    pub(super) vertex_label_index: NativeMap<String, NativeSet<VertexId>>,
}

impl Partition {
    pub(super) fn into_mutation_budget(self, memory: &MemoryBudget) -> Self {
        Self {
            vertex_ids: self.vertex_ids.into_mutation_budget(memory),
            edge_ids: self.edge_ids.into_mutation_budget(memory),
            adj_out: self.adj_out.into_mutation_budget(memory),
            adj_in: self.adj_in.into_mutation_budget(memory),
            label_index: self.label_index.into_mutation_budget(memory),
            vertex_label_index: self.vertex_label_index.into_mutation_budget(memory),
        }
    }

    pub(super) fn new(memory: &MemoryBudget) -> Self {
        Self {
            vertex_ids: NativeSet::new(memory),
            edge_ids: NativeSet::new(memory),
            adj_out: NativeMap::new(memory),
            adj_in: NativeMap::new(memory),
            label_index: NativeMap::new(memory),
            vertex_label_index: NativeMap::new(memory),
        }
    }

    pub(super) fn retained_bytes(&self) -> Result<usize, MemoryError> {
        [
            self.vertex_ids.retained_bytes()?,
            self.edge_ids.retained_bytes()?,
            self.adj_out.retained_bytes()?,
            self.adj_in.retained_bytes()?,
            self.label_index.retained_bytes()?,
            self.vertex_label_index.retained_bytes()?,
        ]
        .into_iter()
        .try_fold(0_usize, |sum, bytes| {
            sum.checked_add(bytes).ok_or(MemoryError::SizeOverflow)
        })
    }

    pub(super) fn add_vertex(&mut self, vertex: &Vertex) -> Result<(), MemoryError> {
        crate::image_work::inline(self.add_vertex_with(vertex, &mut ()))
    }

    pub(super) async fn add_vertex_with<E: From<MemoryError>>(
        &mut self,
        vertex: &Vertex,
        work: &mut impl crate::image_work::Work<E>,
    ) -> Result<(), E> {
        self.vertex_ids.insert(vertex.vertex_id, 0)?;
        update_label_index_with(
            &mut self.vertex_label_index,
            &vertex.label,
            vertex.vertex_id,
            true,
            work,
        )
        .await
    }

    pub(super) fn remove_vertex(&mut self, vertex: &Vertex) -> Result<(), MemoryError> {
        self.vertex_ids.remove(&vertex.vertex_id)?;
        update_label_index(
            &mut self.vertex_label_index,
            &vertex.label,
            vertex.vertex_id,
            false,
        )
    }

    pub(super) fn add_edge(&mut self, edge: &Edge) -> Result<(), MemoryError> {
        crate::image_work::inline(self.add_edge_with(edge, &mut ()))
    }

    pub(super) async fn add_edge_with<E: From<MemoryError>>(
        &mut self,
        edge: &Edge,
        work: &mut impl crate::image_work::Work<E>,
    ) -> Result<(), E> {
        self.edge_ids.insert(edge.edge_id, 0)?;
        update_index(&mut self.adj_out, edge.source_id, edge.edge_id, true)?;
        update_index(&mut self.adj_in, edge.target_id, edge.edge_id, true)?;
        update_label_index_with(&mut self.label_index, &edge.label, edge.edge_id, true, work).await
    }

    pub(super) fn remove_edge(&mut self, edge: &Edge) -> Result<(), MemoryError> {
        self.edge_ids.remove(&edge.edge_id)?;
        update_index(&mut self.adj_out, edge.source_id, edge.edge_id, false)?;
        update_index(&mut self.adj_in, edge.target_id, edge.edge_id, false)?;
        update_label_index(&mut self.label_index, &edge.label, edge.edge_id, false)
    }
}

fn update_index(
    map: &mut NativeMap<u64, NativeSet<u64>>,
    key: u64,
    id: u64,
    add: bool,
) -> Result<(), MemoryError> {
    let existing = map.get(&key);
    if !add && existing.is_none() {
        return Ok(());
    }
    let mut ids = existing
        .cloned()
        .unwrap_or_else(|| NativeSet::new(map.budget()))
        .into_mutation_budget(map.budget());
    if add {
        ids.insert(id, 0)?;
    } else {
        ids.remove(&id)?;
    }
    let retained = ids.retained_bytes()?;
    map.insert(key, ids, 0, retained)
}

fn update_label_index(
    map: &mut NativeMap<String, NativeSet<u64>>,
    label: &str,
    id: u64,
    add: bool,
) -> Result<(), MemoryError> {
    crate::image_work::inline(update_label_index_with(map, label, id, add, &mut ()))
}

async fn update_label_index_with<E: From<MemoryError>>(
    map: &mut NativeMap<String, NativeSet<u64>>,
    label: &str,
    id: u64,
    add: bool,
    work: &mut impl crate::image_work::Work<E>,
) -> Result<(), E> {
    let existing = map.get(label);
    if !add && existing.is_none() {
        return Ok(());
    }
    let mut ids = existing
        .cloned()
        .unwrap_or_else(|| NativeSet::new(map.budget()))
        .into_mutation_budget(map.budget());
    if add {
        ids.insert(id, 0)?;
    } else {
        ids.remove(&id)?;
    }
    let bytes = ids.retained_bytes()?;
    map.insert_named_with(label, ids, 0, bytes, work).await
}
