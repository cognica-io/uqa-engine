//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! In-memory state management and persistence hydration helpers.

use super::{
    BTreeSet, Edge, EdgeId, GraphLabelRegistry, GraphStoreError, GraphStoreResult,
    MemoryGraphStore, Partition, Vertex, VertexId,
};

impl MemoryGraphStore {
    pub fn new() -> Self {
        Self {
            next_vertex_id: 1,
            next_edge_id: 1,
            ..Self::default()
        }
    }

    #[cfg(test)]
    pub(crate) fn remove_edge_record_for_corruption_test(&mut self, edge_id: EdgeId) {
        self.edges.remove(&edge_id);
    }

    #[cfg(test)]
    pub(super) fn remove_vertex_record_for_corruption_test(&mut self, vertex_id: VertexId) {
        self.vertices.remove(&vertex_id);
    }

    pub(super) fn require_partition_mut(&mut self, name: &str) -> GraphStoreResult<&mut Partition> {
        self.graphs
            .get_mut(name)
            .ok_or_else(|| GraphStoreError::UnknownGraph(name.to_string()))
    }

    pub(super) fn require_partition(&self, name: &str) -> GraphStoreResult<&Partition> {
        self.graphs
            .get(name)
            .ok_or_else(|| GraphStoreError::UnknownGraph(name.to_string()))
    }

    pub(super) fn require_query_vertex(
        &self,
        partition: &Partition,
        vertex_id: VertexId,
        graph: &str,
    ) -> GraphStoreResult<()> {
        if !partition.vertex_ids.contains(&vertex_id) {
            return Err(GraphStoreError::InvalidQuery(format!(
                "vertex {vertex_id} is not a member of graph {graph:?}"
            )));
        }
        self.require_partition_vertex(partition, vertex_id, graph)
            .map(|_| ())
    }

    pub(super) fn require_partition_vertex<'a>(
        &'a self,
        partition: &Partition,
        vertex_id: VertexId,
        graph: &str,
    ) -> GraphStoreResult<&'a Vertex> {
        if !partition.vertex_ids.contains(&vertex_id) {
            return Err(GraphStoreError::CorruptGraph(format!(
                "graph {graph:?} references vertex {vertex_id} outside its membership set"
            )));
        }
        self.vertices.get(&vertex_id).ok_or_else(|| {
            GraphStoreError::CorruptGraph(format!(
                "graph {graph:?} references missing vertex {vertex_id}"
            ))
        })
    }

    pub(super) fn require_partition_edge<'a>(
        &'a self,
        partition: &Partition,
        edge_id: EdgeId,
        graph: &str,
    ) -> GraphStoreResult<&'a Edge> {
        if !partition.edge_ids.contains(&edge_id) {
            return Err(GraphStoreError::CorruptGraph(format!(
                "graph {graph:?} adjacency references edge {edge_id} outside its membership set"
            )));
        }
        let edge = self.edges.get(&edge_id).ok_or_else(|| {
            GraphStoreError::CorruptGraph(format!(
                "graph {graph:?} references missing edge {edge_id}"
            ))
        })?;
        self.require_partition_vertex(partition, edge.source_id, graph)?;
        self.require_partition_vertex(partition, edge.target_id, graph)?;
        Ok(edge)
    }

    pub(super) fn ensure_partition(&mut self, name: &str) {
        if !self.graphs.contains_key(name) {
            self.graphs.insert(name.to_string(), Partition::default());
        }
    }

    pub(super) fn release_vertex_if_orphan(&mut self, vertex_id: VertexId) {
        let still_referenced = self
            .vertex_membership
            .get(&vertex_id)
            .is_some_and(|set| !set.is_empty());
        if !still_referenced {
            self.vertices.remove(&vertex_id);
            self.vertex_membership.remove(&vertex_id);
        }
    }

    pub(super) fn release_edge_if_orphan(&mut self, edge_id: EdgeId) {
        let still_referenced = self
            .edge_membership
            .get(&edge_id)
            .is_some_and(|set| !set.is_empty());
        if !still_referenced {
            self.edges.remove(&edge_id);
            self.edge_membership.remove(&edge_id);
        }
    }

    pub(super) fn populate_graph_from_ids(
        &mut self,
        vertex_ids: &BTreeSet<VertexId>,
        edge_ids: &BTreeSet<EdgeId>,
        target: &str,
    ) -> GraphStoreResult<()> {
        let vertices = vertex_ids
            .iter()
            .map(|id| {
                self.vertices.get(id).cloned().ok_or_else(|| {
                    GraphStoreError::CorruptGraph(format!(
                        "graph membership references missing vertex {id}"
                    ))
                })
            })
            .collect::<GraphStoreResult<Vec<_>>>()?;
        let edges = edge_ids
            .iter()
            .map(|id| {
                self.edges.get(id).cloned().ok_or_else(|| {
                    GraphStoreError::CorruptGraph(format!(
                        "graph membership references missing edge {id}"
                    ))
                })
            })
            .collect::<GraphStoreResult<Vec<_>>>()?;

        self.ensure_partition(target);
        for vertex in vertices {
            let id = vertex.vertex_id;
            self.require_partition_mut(target)?.add_vertex(&vertex);
            self.vertex_membership
                .entry(id)
                .or_default()
                .insert(target.to_string());
        }
        for edge in edges {
            let id = edge.edge_id;
            self.require_partition_mut(target)?.add_edge(&edge);
            self.edge_membership
                .entry(id)
                .or_default()
                .insert(target.to_string());
        }
        Ok(())
    }

    /// Insert a vertex into the global registry without attaching it
    /// to any graph. Used by the `SQLite`-backed store on hydration
    /// before membership is restored from the persisted catalog.
    pub fn insert_raw_vertex(&mut self, vertex: Vertex) -> GraphStoreResult<()> {
        let next = if vertex.vertex_id >= self.next_vertex_id {
            vertex.vertex_id.checked_add(1).ok_or_else(|| {
                GraphStoreError::IdExhausted("raw vertex id counter overflow".into())
            })?
        } else {
            self.next_vertex_id
        };
        self.vertices.insert(vertex.vertex_id, vertex);
        self.next_vertex_id = next;
        Ok(())
    }

    pub fn insert_raw_edge(&mut self, edge: Edge) -> GraphStoreResult<()> {
        if !self.vertices.contains_key(&edge.source_id)
            || !self.vertices.contains_key(&edge.target_id)
        {
            return Err(GraphStoreError::CorruptGraph(format!(
                "raw edge {} references missing endpoint {} -> {}",
                edge.edge_id, edge.source_id, edge.target_id
            )));
        }
        let next = if edge.edge_id >= self.next_edge_id {
            edge.edge_id.checked_add(1).ok_or_else(|| {
                GraphStoreError::IdExhausted("raw edge id counter overflow".into())
            })?
        } else {
            self.next_edge_id
        };
        self.edges.insert(edge.edge_id, edge);
        self.next_edge_id = next;
        Ok(())
    }

    /// Attach a previously inserted vertex to the named graph. The
    /// graph must already exist (`create_graph`).
    pub fn attach_vertex(&mut self, vertex_id: VertexId, graph: &str) -> GraphStoreResult<()> {
        if !self.vertices.contains_key(&vertex_id) {
            return Err(GraphStoreError::CorruptGraph(format!(
                "cannot attach missing vertex {vertex_id}"
            )));
        }
        let part = self.require_partition_mut(graph)?;
        part.vertex_ids.insert(vertex_id);
        self.vertex_membership
            .entry(vertex_id)
            .or_default()
            .insert(graph.to_string());
        Ok(())
    }

    pub fn attach_edge(&mut self, edge_id: EdgeId, graph: &str) -> GraphStoreResult<()> {
        // Snapshot the edge fields so we can populate the partition's
        // adjacency indexes without re-borrowing `self.edges`.
        let edge = self.edges.get(&edge_id).cloned().ok_or_else(|| {
            GraphStoreError::CorruptGraph(format!("cannot attach missing edge {edge_id}"))
        })?;
        let partition = self.require_partition(graph)?;
        self.require_partition_vertex(partition, edge.source_id, graph)?;
        self.require_partition_vertex(partition, edge.target_id, graph)?;
        let part = self.require_partition_mut(graph)?;
        part.add_edge(&edge);
        self.edge_membership
            .entry(edge_id)
            .or_default()
            .insert(graph.to_string());
        Ok(())
    }

    /// All edge ids that participate in `graph`, in stable id order.
    /// Helper for the `SQLite`-backed store's bulk write paths.
    pub fn out_edge_ids_for_graph(&self, graph: &str) -> GraphStoreResult<BTreeSet<EdgeId>> {
        Ok(self.require_partition(graph)?.edge_ids.clone())
    }

    /// Snapshot of the AGE label registry for `graph` (empty registry
    /// when the graph has never allocated an id).
    pub fn label_registry(&self, graph: &str) -> GraphLabelRegistry {
        self.label_registries
            .get(graph)
            .cloned()
            .unwrap_or_default()
    }

    /// Install (merge) a persisted label registry for `graph`. Existing
    /// in-memory state wins on conflicts except that the larger
    /// sequence / label-id watermarks are kept.
    pub fn import_label_registry(&mut self, graph: &str, registry: &GraphLabelRegistry) {
        self.label_registries
            .entry(graph.to_string())
            .or_default()
            .merge(registry);
    }

    /// Re-derive the label registry of `graph` from the ids of the
    /// entities currently attached to it. Self-heals restored graphs
    /// whose registry metadata is missing: `label_id = id >> 48`.
    pub fn rebuild_label_registry_from_ids(&mut self, graph: &str) {
        let mut observations: Vec<(String, u64)> = Vec::new();
        if let Some(part) = self.graphs.get(graph) {
            for vid in &part.vertex_ids {
                if let Some(vertex) = self.vertices.get(vid) {
                    observations.push((vertex.label.clone(), vertex.vertex_id));
                }
            }
            for eid in &part.edge_ids {
                if let Some(edge) = self.edges.get(eid) {
                    observations.push((edge.label.clone(), edge.edge_id));
                }
            }
        }
        let registry = self.label_registries.entry(graph.to_string()).or_default();
        for (label, id) in observations {
            registry.observe(&label, id);
        }
    }
}
