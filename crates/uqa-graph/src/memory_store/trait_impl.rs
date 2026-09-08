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
            .iter()
            .copied()
            .filter(|id| after.is_none_or(|after| *id > after))
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
            .iter()
            .copied()
            .filter(|id| after.is_none_or(|after| *id > after))
            .take(limit)
            .collect())
    }
    fn transaction<T>(
        &mut self,
        operation: impl FnOnce(&mut Self) -> GraphStoreResult<T>,
    ) -> GraphStoreResult<T> {
        let backup = self.clone();
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| operation(self))) {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(error)) => {
                *self = backup;
                Err(error)
            }
            Err(panic) => {
                *self = backup;
                std::panic::resume_unwind(panic)
            }
        }
    }

    fn create_graph(&mut self, name: &str) -> GraphStoreResult<()> {
        MemoryGraphStore::create_graph(self, name);
        Ok(())
    }

    fn drop_graph(&mut self, name: &str) -> GraphStoreResult<()> {
        MemoryGraphStore::drop_graph(self, name);
        Ok(())
    }

    fn graph_names(&self) -> GraphStoreResult<Vec<String>> {
        Ok(MemoryGraphStore::graph_names(self))
    }

    fn has_graph(&self, name: &str) -> GraphStoreResult<bool> {
        Ok(MemoryGraphStore::has_graph(self, name))
    }

    fn union_graphs(&mut self, g1: &str, g2: &str, target: &str) -> GraphStoreResult<()> {
        let v_union: BTreeSet<VertexId> = self
            .require_partition(g1)?
            .vertex_ids
            .union(&self.require_partition(g2)?.vertex_ids)
            .copied()
            .collect();
        let e_union: BTreeSet<EdgeId> = self
            .require_partition(g1)?
            .edge_ids
            .union(&self.require_partition(g2)?.edge_ids)
            .copied()
            .collect();
        self.populate_graph_from_ids(&v_union, &e_union, target)
    }

    fn intersect_graphs(&mut self, g1: &str, g2: &str, target: &str) -> GraphStoreResult<()> {
        let v_inter: BTreeSet<VertexId> = self
            .require_partition(g1)?
            .vertex_ids
            .intersection(&self.require_partition(g2)?.vertex_ids)
            .copied()
            .collect();
        let e_inter: BTreeSet<EdgeId> = self
            .require_partition(g1)?
            .edge_ids
            .intersection(&self.require_partition(g2)?.edge_ids)
            .copied()
            .collect();
        self.populate_graph_from_ids(&v_inter, &e_inter, target)
    }

    fn difference_graphs(&mut self, g1: &str, g2: &str, target: &str) -> GraphStoreResult<()> {
        let v_diff: BTreeSet<VertexId> = self
            .require_partition(g1)?
            .vertex_ids
            .difference(&self.require_partition(g2)?.vertex_ids)
            .copied()
            .collect();
        let e_diff: BTreeSet<EdgeId> = self
            .require_partition(g1)?
            .edge_ids
            .difference(&self.require_partition(g2)?.edge_ids)
            .copied()
            .collect();
        self.populate_graph_from_ids(&v_diff, &e_diff, target)
    }

    fn copy_graph(&mut self, source: &str, target: &str) -> GraphStoreResult<()> {
        let v_copy = self.require_partition(source)?.vertex_ids.clone();
        let e_copy = self.require_partition(source)?.edge_ids.clone();
        self.populate_graph_from_ids(&v_copy, &e_copy, target)
    }

    fn add_vertex(&mut self, vertex: Vertex, graph: &str) -> GraphStoreResult<()> {
        self.require_partition(graph)?;
        let vid = vertex.vertex_id;
        let next_vertex_id = if vid >= self.next_vertex_id {
            vid.checked_add(1).ok_or_else(|| {
                GraphStoreError::IdExhausted("vertex id counter overflow".to_string())
            })?
        } else {
            self.next_vertex_id
        };
        let mut owners = self
            .vertex_membership
            .get(&vid)
            .cloned()
            .unwrap_or_default();
        owners.insert(graph.to_owned());
        for owner in &owners {
            self.require_partition(owner)?;
        }
        let previous = self.vertices.insert(vid, vertex.clone());
        for owner in &owners {
            let partition = self.graphs.get_mut(owner).expect("validated vertex owner");
            if let Some(previous) = &previous {
                if let Some(ids) = partition.vertex_label_index.get_mut(&previous.label) {
                    ids.remove(&vid);
                }
            }
            partition.add_vertex(&vertex);
        }
        self.vertex_membership.insert(vid, owners);
        self.next_vertex_id = next_vertex_id;
        Ok(())
    }

    fn add_edge(&mut self, edge: Edge, graph: &str) -> GraphStoreResult<()> {
        let mut owners = self
            .edge_membership
            .get(&edge.edge_id)
            .cloned()
            .unwrap_or_default();
        owners.insert(graph.to_owned());
        for owner in &owners {
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
        let eid = edge.edge_id;
        let next_edge_id = if eid >= self.next_edge_id {
            eid.checked_add(1).ok_or_else(|| {
                GraphStoreError::IdExhausted("edge id counter overflow".to_string())
            })?
        } else {
            self.next_edge_id
        };
        let previous = self.edges.insert(eid, edge.clone());
        for owner in &owners {
            let partition = self.graphs.get_mut(owner).expect("validated edge owner");
            if let Some(previous) = &previous {
                partition.remove_edge(previous);
            }
            partition.add_edge(&edge);
        }
        self.edge_membership.insert(eid, owners);
        self.next_edge_id = next_edge_id;
        Ok(())
    }

    fn remove_vertex(&mut self, vertex_id: VertexId, graph: &str) -> GraphStoreResult<()> {
        let partition = self.require_partition_mut(graph)?;
        if !partition.vertex_ids.remove(&vertex_id) {
            return Ok(());
        }
        for vids in partition.vertex_label_index.values_mut() {
            vids.remove(&vertex_id);
        }
        // Remove all incident edges from this graph.
        let out_edges: Vec<EdgeId> = partition
            .adj_out
            .remove(&vertex_id)
            .map(|s| s.into_iter().collect())
            .unwrap_or_default();
        let in_edges: Vec<EdgeId> = partition
            .adj_in
            .remove(&vertex_id)
            .map(|s| s.into_iter().collect())
            .unwrap_or_default();
        let edge_ids_to_drop: Vec<EdgeId> = out_edges.into_iter().chain(in_edges).collect();
        for eid in &edge_ids_to_drop {
            if let Some(edge) = self.edges.get(eid).cloned() {
                if let Some(part) = self.graphs.get_mut(graph) {
                    part.remove_edge(&edge);
                }
                if let Some(set) = self.edge_membership.get_mut(eid) {
                    set.remove(graph);
                }
            }
        }
        if let Some(set) = self.vertex_membership.get_mut(&vertex_id) {
            set.remove(graph);
        }
        for eid in edge_ids_to_drop {
            self.release_edge_if_orphan(eid);
        }
        self.release_vertex_if_orphan(vertex_id);
        Ok(())
    }

    fn remove_edge(&mut self, edge_id: EdgeId, graph: &str) -> GraphStoreResult<()> {
        self.require_partition(graph)?;
        let Some(edge) = self.edges.get(&edge_id).cloned() else {
            return Ok(());
        };
        let partition = self.require_partition_mut(graph)?;
        if !partition.edge_ids.contains(&edge_id) {
            return Ok(());
        }
        partition.remove_edge(&edge);
        if let Some(set) = self.edge_membership.get_mut(&edge_id) {
            set.remove(graph);
        }
        self.release_edge_if_orphan(edge_id);
        Ok(())
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
        let mut collect = |set: &BTreeSet<EdgeId>, take_target: bool| -> GraphStoreResult<()> {
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
            .cloned()
            .unwrap_or_default())
    }

    fn out_edge_ids(&self, vertex_id: VertexId, graph: &str) -> GraphStoreResult<BTreeSet<EdgeId>> {
        let partition = self.require_partition(graph)?;
        self.require_query_vertex(partition, vertex_id, graph)?;
        let edges = partition
            .adj_out
            .get(&vertex_id)
            .cloned()
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
            .cloned()
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
            .cloned()
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
        Ok(partition.vertex_ids.clone())
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
            let degree = u64::try_from(edge_ids.map_or(0, BTreeSet::len)).map_err(|_| {
                GraphStoreError::CorruptGraph(format!("out-degree for vertex {vid} exceeds u64"))
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
        let mut candidate = self
            .label_registries
            .get(graph)
            .cloned()
            .unwrap_or_default();
        let label_id = candidate.label_id(label, LabelKind::Vertex)?;
        let id = make_graphid(label_id, candidate.next_sequence(label_id)?)?;
        self.label_registries.insert(graph.to_string(), candidate);
        Ok(id)
    }

    fn allocate_edge_id(&mut self, label: &str, graph: &str) -> GraphStoreResult<EdgeId> {
        if !self.has_graph(graph) {
            return Err(GraphStoreError::UnknownGraph(graph.to_string()));
        }
        let mut candidate = self
            .label_registries
            .get(graph)
            .cloned()
            .unwrap_or_default();
        let label_id = candidate.label_id(label, LabelKind::Edge)?;
        let id = make_graphid(label_id, candidate.next_sequence(label_id)?)?;
        self.label_registries.insert(graph.to_string(), candidate);
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

// Concrete memory-only accessors remain infallible; generic graph execution
// uses the fallible, owned-record storage interface above.
impl MemoryGraphStore {
    pub fn create_graph(&mut self, name: &str) {
        self.graphs.entry(name.to_string()).or_default();
    }

    pub fn drop_graph(&mut self, name: &str) {
        // AGE drops every per-graph label table with the graph, so a
        // re-created graph starts a fresh label / sequence space.
        self.label_registries.remove(name);
        let Some(partition) = self.graphs.remove(name) else {
            return;
        };
        for vid in &partition.vertex_ids {
            if let Some(set) = self.vertex_membership.get_mut(vid) {
                set.remove(name);
            }
        }
        for eid in &partition.edge_ids {
            if let Some(set) = self.edge_membership.get_mut(eid) {
                set.remove(name);
            }
        }
        // Drop any vertex / edge that no longer has any membership.
        let orphan_vertices: Vec<VertexId> = partition.vertex_ids.iter().copied().collect();
        for vid in orphan_vertices {
            self.release_vertex_if_orphan(vid);
        }
        let orphan_edges: Vec<EdgeId> = partition.edge_ids.iter().copied().collect();
        for eid in orphan_edges {
            self.release_edge_if_orphan(eid);
        }
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
            .cloned()
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
        self.vertices.clone()
    }

    pub fn edges(&self) -> BTreeMap<EdgeId, Edge> {
        self.edges.clone()
    }
}
