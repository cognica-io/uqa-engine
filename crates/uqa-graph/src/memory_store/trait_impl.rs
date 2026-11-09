//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `GraphStore` mutation, traversal, statistics, and id-allocation contract.

use super::{
    make_graphid, usize_to_f64_exact, BTreeMap, BTreeSet, Direction, Edge, EdgeId, GraphStore,
    GraphStoreError, GraphStoreResult, LabelKind, MemoryGraphStore, Vertex, VertexId,
};

impl GraphStore for MemoryGraphStore {
    fn vertex_id_page(
        &self,
        graph: &str,
        after: Option<u64>,
        limit: usize,
    ) -> GraphStoreResult<Vec<u64>> {
        if !(1..=uqa_storage::MAX_GRAPH_ID_PAGE).contains(&limit) {
            return Err(GraphStoreError::InvalidQuery(
                "invalid graph page size".into(),
            ));
        }
        Ok(self
            .require_partition(graph)?
            .vertex_ids
            .range_from(
                after
                    .as_ref()
                    .map_or(std::ops::Bound::Unbounded, std::ops::Bound::Excluded),
            )
            .copied()
            .take(limit)
            .collect())
    }
    fn edge_id_page(
        &self,
        graph: &str,
        after: Option<u64>,
        limit: usize,
    ) -> GraphStoreResult<Vec<u64>> {
        if !(1..=uqa_storage::MAX_GRAPH_ID_PAGE).contains(&limit) {
            return Err(GraphStoreError::InvalidQuery(
                "invalid graph page size".into(),
            ));
        }
        Ok(self
            .require_partition(graph)?
            .edge_ids
            .range_from(
                after
                    .as_ref()
                    .map_or(std::ops::Bound::Unbounded, std::ops::Bound::Excluded),
            )
            .copied()
            .take(limit)
            .collect())
    }
    fn transaction<T>(
        &mut self,
        operation: impl FnOnce(&mut Self) -> GraphStoreResult<T>,
    ) -> GraphStoreResult<T> {
        let mut backup = self.clone();
        backup.image_memory = self.image_memory.take();
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| operation(self)));
        if matches!(&outcome, Ok(Ok(_))) {
            self.image_memory = backup.image_memory.take();
        } else {
            *self = backup;
        }
        match outcome {
            Ok(result) => result,
            Err(panic) => std::panic::resume_unwind(panic),
        }
    }

    fn create_graph(&mut self, name: &str) -> GraphStoreResult<()> {
        MemoryGraphStore::create_graph(self, name)
    }

    fn drop_graph(&mut self, name: &str) -> GraphStoreResult<()> {
        MemoryGraphStore::drop_graph(self, name)
    }

    fn graph_names(&self) -> GraphStoreResult<Vec<String>> {
        Ok(MemoryGraphStore::graph_names(self))
    }

    fn has_graph(&self, name: &str) -> GraphStoreResult<bool> {
        Ok(MemoryGraphStore::has_graph(self, name))
    }

    fn union_graphs(&mut self, g1: &str, g2: &str, target: &str) -> GraphStoreResult<()> {
        let left = self.require_partition(g1)?;
        let right = self.require_partition(g2)?;
        let vertices = left
            .vertex_ids
            .iter()
            .chain(right.vertex_ids.iter())
            .copied()
            .collect();
        let edges = left
            .edge_ids
            .iter()
            .chain(right.edge_ids.iter())
            .copied()
            .collect();
        self.populate_graph_from_ids(&vertices, &edges, target)
    }

    fn intersect_graphs(&mut self, g1: &str, g2: &str, target: &str) -> GraphStoreResult<()> {
        let left = self.require_partition(g1)?;
        let right = self.require_partition(g2)?;
        let vertices = left
            .vertex_ids
            .iter()
            .filter(|id| right.vertex_ids.contains(*id))
            .copied()
            .collect();
        let edges = left
            .edge_ids
            .iter()
            .filter(|id| right.edge_ids.contains(*id))
            .copied()
            .collect();
        self.populate_graph_from_ids(&vertices, &edges, target)
    }

    fn difference_graphs(&mut self, g1: &str, g2: &str, target: &str) -> GraphStoreResult<()> {
        let left = self.require_partition(g1)?;
        let right = self.require_partition(g2)?;
        let vertices = left
            .vertex_ids
            .iter()
            .filter(|id| !right.vertex_ids.contains(*id))
            .copied()
            .collect();
        let edges = left
            .edge_ids
            .iter()
            .filter(|id| !right.edge_ids.contains(*id))
            .copied()
            .collect();
        self.populate_graph_from_ids(&vertices, &edges, target)
    }

    fn copy_graph(&mut self, source: &str, target: &str) -> GraphStoreResult<()> {
        let v_copy = self.require_partition(source)?.vertex_ids.to_set();
        let e_copy = self.require_partition(source)?.edge_ids.to_set();
        self.populate_graph_from_ids(&v_copy, &e_copy, target)
    }

    fn add_vertex(&mut self, vertex: Vertex, graph: &str) -> GraphStoreResult<()> {
        self.require_partition(graph)?;
        let vid = vertex.vertex_id;
        let next = if vid >= self.next_vertex_id {
            vid.checked_add(1)
                .ok_or_else(|| GraphStoreError::IdExhausted("vertex id counter overflow".into()))?
        } else {
            self.next_vertex_id
        };
        if let Some(owners) = self.vertex_membership.get(&vid) {
            for owner in owners {
                if owner != graph {
                    self.require_partition(owner)?;
                }
            }
        }
        self.publish(|state| {
            super::publication::membership(&mut state.vertex_membership, vid, graph, true)?;
            let owners = state
                .vertex_membership
                .get(&vid)
                .expect("prepared membership")
                .clone();
            for owner in &owners {
                let mut partition = state.partition_candidate(owner)?;
                if let Some(previous) = state.vertices.get(&vid) {
                    partition.remove_vertex(previous)?;
                }
                partition.add_vertex(&vertex)?;
                state.put_partition(owner, partition)?;
            }
            let bytes = super::publication::entity_bytes(
                &vertex.label,
                &vertex.properties,
                state.vertices.budget(),
            )?;
            state.vertices.insert(vid, vertex, bytes, bytes)?;
            state.next_vertex_id = next;
            Ok(())
        })
    }

    fn add_edge(&mut self, edge: Edge, graph: &str) -> GraphStoreResult<()> {
        let eid = edge.edge_id;
        for owner in super::publication::owners(self.edge_membership.get(&eid), graph) {
            let partition = self.require_partition(owner)?;
            if !partition.vertex_ids.contains(&edge.source_id)
                || !partition.vertex_ids.contains(&edge.target_id)
            {
                return Err(GraphStoreError::InvalidMutation(format!(
                    "edge {} references endpoint outside graph {owner:?}: {} -> {}",
                    edge.edge_id, edge.source_id, edge.target_id
                )));
            }
            self.require_partition_vertex(partition, edge.source_id, owner)?;
            self.require_partition_vertex(partition, edge.target_id, owner)?;
        }
        let next = if eid >= self.next_edge_id {
            eid.checked_add(1)
                .ok_or_else(|| GraphStoreError::IdExhausted("edge id counter overflow".into()))?
        } else {
            self.next_edge_id
        };
        self.publish(|state| {
            super::publication::membership(&mut state.edge_membership, eid, graph, true)?;
            let owners = state
                .edge_membership
                .get(&eid)
                .expect("prepared membership")
                .clone();
            for owner in &owners {
                let mut partition = state.partition_candidate(owner)?;
                if let Some(previous) = state.edges.get(&eid) {
                    partition.remove_edge(previous)?;
                }
                partition.add_edge(&edge)?;
                state.put_partition(owner, partition)?;
            }
            let bytes = super::publication::entity_bytes(
                &edge.label,
                &edge.properties,
                state.edges.budget(),
            )?;
            state.edges.insert(eid, edge, bytes, bytes)?;
            state.next_edge_id = next;
            Ok(())
        })
    }

    fn remove_vertex(&mut self, vertex_id: VertexId, graph: &str) -> GraphStoreResult<()> {
        self.publish(|state| {
            let mut partition = state.partition_candidate(graph)?;
            if !partition.vertex_ids.contains(&vertex_id) {
                return Ok(());
            }
            let vertex = state.require_partition_vertex(&partition, vertex_id, graph)?;
            partition.remove_vertex(vertex)?;
            let mut incident = BTreeSet::new();
            for index in [&partition.adj_out, &partition.adj_in] {
                if let Some(ids) = index.get(&vertex_id) {
                    incident.extend(ids.iter().copied());
                }
            }
            partition.adj_out.remove(&vertex_id)?;
            partition.adj_in.remove(&vertex_id)?;
            for id in incident {
                if let Some(edge) = state.edges.get(&id) {
                    partition.remove_edge(edge)?;
                    super::publication::membership(&mut state.edge_membership, id, graph, false)?;
                }
                state.release_edge_if_orphan(id)?;
            }
            super::publication::membership(&mut state.vertex_membership, vertex_id, graph, false)?;
            state.release_vertex_if_orphan(vertex_id)?;
            state.put_partition(graph, partition)
        })
    }

    fn remove_edge(&mut self, edge_id: EdgeId, graph: &str) -> GraphStoreResult<()> {
        self.publish(|state| {
            let mut partition = state.partition_candidate(graph)?;
            let Some(edge) = state.edges.get(&edge_id) else {
                return Ok(());
            };
            if !partition.edge_ids.contains(&edge_id) {
                return Ok(());
            }
            partition.remove_edge(edge)?;
            super::publication::membership(&mut state.edge_membership, edge_id, graph, false)?;
            state.release_edge_if_orphan(edge_id)?;
            state.put_partition(graph, partition)
        })
    }

    fn neighbors(
        &self,
        vertex_id: VertexId,
        label: Option<&str>,
        direction: Direction,
        graph: &str,
    ) -> GraphStoreResult<Vec<VertexId>> {
        let partition = self.require_partition(graph)?;
        self.require_query_vertex(partition, vertex_id, graph)?;
        let mut result = Vec::new();
        let mut collect = |set: &super::NativeSet<EdgeId>,
                           take_target: bool|
         -> GraphStoreResult<()> {
            for eid in set {
                let edge = self.require_partition_edge(partition, *eid, graph)?;
                let expected_endpoint = if take_target {
                    edge.source_id
                } else {
                    edge.target_id
                };
                if expected_endpoint != vertex_id {
                    return Err(GraphStoreError::CorruptGraph(format!(
                        "graph {graph:?} adjacency for vertex {vertex_id} references edge {eid} with endpoints {} -> {}",
                        edge.source_id, edge.target_id
                    )));
                }
                if let Some(want) = label {
                    if edge.label != want {
                        continue;
                    }
                }
                result.push(if take_target {
                    edge.target_id
                } else {
                    edge.source_id
                });
            }
            Ok(())
        };
        match direction {
            Direction::Out => {
                if let Some(set) = partition.adj_out.get(&vertex_id) {
                    collect(set, true)?;
                }
            }
            Direction::In => {
                if let Some(set) = partition.adj_in.get(&vertex_id) {
                    collect(set, false)?;
                }
            }
            Direction::Both => {
                if let Some(set) = partition.adj_out.get(&vertex_id) {
                    collect(set, true)?;
                }
                if let Some(set) = partition.adj_in.get(&vertex_id) {
                    collect(set, false)?;
                }
                result.sort_unstable();
                result.dedup();
            }
        }
        Ok(result)
    }

    fn vertices_by_label(&self, label: &str, graph: &str) -> GraphStoreResult<Vec<Vertex>> {
        let partition = self.require_partition(graph)?;
        partition
            .vertex_label_index
            .get(label)
            .into_iter()
            .flat_map(|set| set.iter())
            .map(|vertex_id| {
                self.require_partition_vertex(partition, *vertex_id, graph)
                    .cloned()
            })
            .collect()
    }

    fn vertex_ids_by_label(&self, label: &str, graph: &str) -> GraphStoreResult<Vec<VertexId>> {
        let partition = self.require_partition(graph)?;
        partition
            .vertex_label_index
            .get(label)
            .into_iter()
            .flat_map(|set| set.iter())
            .map(|vertex_id| {
                self.require_partition_vertex(partition, *vertex_id, graph)?;
                Ok(*vertex_id)
            })
            .collect()
    }

    fn vertices_in_graph(&self, graph: &str) -> GraphStoreResult<Vec<Vertex>> {
        let partition = self.require_partition(graph)?;
        partition
            .vertex_ids
            .iter()
            .map(|vertex_id| {
                self.require_partition_vertex(partition, *vertex_id, graph)
                    .cloned()
            })
            .collect()
    }

    fn edges_in_graph(&self, graph: &str) -> GraphStoreResult<Vec<Edge>> {
        let partition = self.require_partition(graph)?;
        partition
            .edge_ids
            .iter()
            .map(|edge_id| {
                self.require_partition_edge(partition, *edge_id, graph)
                    .cloned()
            })
            .collect()
    }

    fn vertex_graphs(&self, vertex_id: VertexId) -> GraphStoreResult<BTreeSet<String>> {
        Ok(MemoryGraphStore::vertex_graphs(self, vertex_id))
    }

    fn edge_graphs(&self, edge_id: EdgeId) -> GraphStoreResult<BTreeSet<String>> {
        Ok(self
            .edge_membership
            .get(&edge_id)
            .map(super::NativeSet::to_set)
            .unwrap_or_default())
    }

    fn out_edge_ids(&self, vertex_id: VertexId, graph: &str) -> GraphStoreResult<BTreeSet<EdgeId>> {
        let partition = self.require_partition(graph)?;
        self.require_query_vertex(partition, vertex_id, graph)?;
        let edges = partition
            .adj_out
            .get(&vertex_id)
            .map(super::NativeSet::to_set)
            .unwrap_or_default();
        for edge_id in &edges {
            let edge = self.require_partition_edge(partition, *edge_id, graph)?;
            if edge.source_id != vertex_id {
                return Err(GraphStoreError::CorruptGraph(format!(
                    "graph {graph:?} outgoing adjacency for vertex {vertex_id} references edge {edge_id} sourced at {}",
                    edge.source_id
                )));
            }
        }
        Ok(edges)
    }

    fn in_edge_ids(&self, vertex_id: VertexId, graph: &str) -> GraphStoreResult<BTreeSet<EdgeId>> {
        let partition = self.require_partition(graph)?;
        self.require_query_vertex(partition, vertex_id, graph)?;
        let edges = partition
            .adj_in
            .get(&vertex_id)
            .map(super::NativeSet::to_set)
            .unwrap_or_default();
        for edge_id in &edges {
            let edge = self.require_partition_edge(partition, *edge_id, graph)?;
            if edge.target_id != vertex_id {
                return Err(GraphStoreError::CorruptGraph(format!(
                    "graph {graph:?} incoming adjacency for vertex {vertex_id} references edge {edge_id} targeted at {}",
                    edge.target_id
                )));
            }
        }
        Ok(edges)
    }

    fn edge_ids_by_label(&self, label: &str, graph: &str) -> GraphStoreResult<BTreeSet<EdgeId>> {
        let partition = self.require_partition(graph)?;
        let edges = partition
            .label_index
            .get(label)
            .map(super::NativeSet::to_set)
            .unwrap_or_default();
        for edge_id in &edges {
            let edge = self.require_partition_edge(partition, *edge_id, graph)?;
            if edge.label != label {
                return Err(GraphStoreError::CorruptGraph(format!(
                    "graph {graph:?} label index {label:?} references edge {edge_id} labelled {:?}",
                    edge.label
                )));
            }
        }
        Ok(edges)
    }

    fn vertex_ids_in_graph(&self, graph: &str) -> GraphStoreResult<BTreeSet<VertexId>> {
        let partition = self.require_partition(graph)?;
        for vertex_id in &partition.vertex_ids {
            self.require_partition_vertex(partition, *vertex_id, graph)?;
        }
        Ok(partition.vertex_ids.to_set())
    }

    fn require_vertex_in_graph(&self, vertex_id: VertexId, graph: &str) -> GraphStoreResult<()> {
        let partition = self.require_partition(graph)?;
        self.require_query_vertex(partition, vertex_id, graph)
    }

    fn degree_distribution(&self, graph: &str) -> GraphStoreResult<BTreeMap<VertexId, u64>> {
        let partition = self.require_partition(graph)?;
        let mut out = BTreeMap::new();
        for vid in &partition.vertex_ids {
            self.require_partition_vertex(partition, *vid, graph)?;
            let edge_ids = partition.adj_out.get(vid);
            if let Some(edge_ids) = edge_ids {
                for edge_id in edge_ids {
                    let edge = self.require_partition_edge(partition, *edge_id, graph)?;
                    if edge.source_id != *vid {
                        return Err(GraphStoreError::CorruptGraph(format!(
                            "graph {graph:?} outgoing adjacency for vertex {vid} references edge {edge_id} sourced at {}",
                            edge.source_id
                        )));
                    }
                }
            }
            let degree =
                u64::try_from(edge_ids.map_or(0, super::NativeSet::len)).map_err(|_| {
                    GraphStoreError::CorruptGraph(format!(
                        "out-degree for vertex {vid} exceeds u64"
                    ))
                })?;
            out.insert(*vid, degree);
        }
        Ok(out)
    }

    fn label_degree(&self, label: &str, graph: &str) -> GraphStoreResult<f64> {
        let partition = self.require_partition(graph)?;
        let Some(eids) = partition.label_index.get(label) else {
            return Ok(0.0);
        };
        if eids.is_empty() {
            return Ok(0.0);
        }
        let mut sources: BTreeSet<VertexId> = BTreeSet::new();
        for eid in eids {
            let edge = self.require_partition_edge(partition, *eid, graph)?;
            if edge.label != label {
                return Err(GraphStoreError::CorruptGraph(format!(
                    "graph {graph:?} label index {label:?} references edge {eid} labelled {:?}",
                    edge.label
                )));
            }
            sources.insert(edge.source_id);
        }
        if sources.is_empty() {
            Ok(0.0)
        } else {
            Ok(usize_to_f64_exact(eids.len(), "edge label count")?
                / usize_to_f64_exact(sources.len(), "edge label source count")?)
        }
    }

    fn vertex_label_counts(&self, graph: &str) -> GraphStoreResult<BTreeMap<String, u64>> {
        let partition = self.require_partition(graph)?;
        let mut out = BTreeMap::new();
        for (label, vids) in &partition.vertex_label_index {
            for vertex_id in vids {
                let vertex = self.require_partition_vertex(partition, *vertex_id, graph)?;
                if vertex.label != *label {
                    return Err(GraphStoreError::CorruptGraph(format!(
                        "graph {graph:?} vertex label index {label:?} references vertex {vertex_id} labelled {:?}",
                        vertex.label
                    )));
                }
            }
            let count = u64::try_from(vids.len()).map_err(|_| {
                GraphStoreError::CorruptGraph(format!(
                    "vertex count for label {label:?} exceeds u64"
                ))
            })?;
            if count > 0 {
                out.insert(label.clone(), count);
            }
        }
        Ok(out)
    }

    fn get_vertex(&self, vertex_id: VertexId) -> GraphStoreResult<Option<Vertex>> {
        Ok(MemoryGraphStore::get_vertex(self, vertex_id).cloned())
    }

    fn for_each_vertex_borrowed(
        &self,
        ids: &[VertexId],
        visit: &mut dyn FnMut(VertexId, Option<&Vertex>) -> bool,
    ) -> GraphStoreResult<Option<usize>> {
        let mut count = 0;
        for id in ids {
            count += 1;
            if !visit(*id, MemoryGraphStore::get_vertex(self, *id)) {
                break;
            }
        }
        Ok(Some(count))
    }

    fn get_edge(&self, edge_id: EdgeId) -> GraphStoreResult<Option<Edge>> {
        Ok(MemoryGraphStore::get_edge(self, edge_id).cloned())
    }

    fn next_vertex_id(&mut self) -> GraphStoreResult<VertexId> {
        let id = self.next_vertex_id;
        self.next_vertex_id = id.checked_add(1).ok_or_else(|| {
            GraphStoreError::IdExhausted("vertex id counter overflow".to_string())
        })?;
        Ok(id)
    }

    fn next_edge_id(&mut self) -> GraphStoreResult<EdgeId> {
        let id = self.next_edge_id;
        self.next_edge_id = id
            .checked_add(1)
            .ok_or_else(|| GraphStoreError::IdExhausted("edge id counter overflow".to_string()))?;
        Ok(id)
    }

    fn allocate_vertex_id(&mut self, label: &str, graph: &str) -> GraphStoreResult<VertexId> {
        if !self.has_graph(graph) {
            return Err(GraphStoreError::UnknownGraph(graph.to_string()));
        }
        let mut candidate = self.native_registry(graph);
        let label_id = candidate.label_id(label, LabelKind::Vertex)?;
        let id = make_graphid(label_id, candidate.next_sequence(label_id)?)?;
        self.put_registry(graph, candidate)?;
        Ok(id)
    }

    fn allocate_edge_id(&mut self, label: &str, graph: &str) -> GraphStoreResult<EdgeId> {
        if !self.has_graph(graph) {
            return Err(GraphStoreError::UnknownGraph(graph.to_string()));
        }
        let mut candidate = self.native_registry(graph);
        let label_id = candidate.label_id(label, LabelKind::Edge)?;
        let id = make_graphid(label_id, candidate.next_sequence(label_id)?)?;
        self.put_registry(graph, candidate)?;
        Ok(id)
    }

    fn clear(&mut self) -> GraphStoreResult<()> {
        MemoryGraphStore::clear(self);
        Ok(())
    }

    fn vertices(&self) -> GraphStoreResult<BTreeMap<VertexId, Vertex>> {
        Ok(MemoryGraphStore::vertices(self))
    }

    fn edges(&self) -> GraphStoreResult<BTreeMap<EdgeId, Edge>> {
        Ok(MemoryGraphStore::edges(self))
    }
}

// Memory accessors borrow native records; mutations retain their typed admission errors.
impl MemoryGraphStore {
    pub fn create_graph(&mut self, name: &str) -> GraphStoreResult<()> {
        self.create_partition(name)
    }

    pub fn drop_graph(&mut self, name: &str) -> GraphStoreResult<()> {
        self.publish(|state| {
            state.label_registries.remove(name)?;
            let Some(partition) = state.graphs.get(name).cloned() else {
                return Ok(());
            };
            for id in &partition.vertex_ids {
                super::publication::membership(&mut state.vertex_membership, *id, name, false)?;
                state.release_vertex_if_orphan(*id)?;
            }
            for id in &partition.edge_ids {
                super::publication::membership(&mut state.edge_membership, *id, name, false)?;
                state.release_edge_if_orphan(*id)?;
            }
            state.graphs.remove(name)?;
            Ok(())
        })
    }

    pub fn graph_names(&self) -> Vec<String> {
        self.graphs.keys().cloned().collect()
    }

    pub fn has_graph(&self, name: &str) -> bool {
        self.graphs.contains_key(name)
    }

    pub fn vertex_graphs(&self, vertex_id: VertexId) -> BTreeSet<String> {
        self.vertex_membership
            .get(&vertex_id)
            .map(super::NativeSet::to_set)
            .unwrap_or_default()
    }

    pub fn get_vertex(&self, vertex_id: VertexId) -> Option<&Vertex> {
        self.vertices.get(&vertex_id)
    }

    pub fn get_edge(&self, edge_id: EdgeId) -> Option<&Edge> {
        self.edges.get(&edge_id)
    }

    pub fn clear(&mut self) {
        self.vertices.clear();
        self.edges.clear();
        self.graphs.clear();
        self.vertex_membership.clear();
        self.edge_membership.clear();
        self.label_registries.clear();
        self.next_vertex_id = 1;
        self.next_edge_id = 1;
    }

    pub fn vertices(&self) -> BTreeMap<VertexId, Vertex> {
        self.vertices
            .iter()
            .map(|(id, vertex)| (*id, vertex.clone()))
            .collect()
    }

    pub fn edges(&self) -> BTreeMap<EdgeId, Edge> {
        self.edges
            .iter()
            .map(|(id, edge)| (*id, edge.clone()))
            .collect()
    }
}
