//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `MemoryGraphStore`: in-memory implementation of [`GraphStore`].
//!
//! Vertex and edge records live in a single global map keyed by id.
//! Each named graph holds a [`Partition`] of ids and adjacency
//! indexes; deleting a graph trims membership and reclaims any record
//! that becomes unreferenced.

use std::collections::{BTreeMap, BTreeSet};

use uqa_core::{Edge, EdgeId, Vertex, VertexId};

use crate::store::GraphStore;
use crate::types::Direction;

#[derive(Debug, Default)]
struct Partition {
    vertex_ids: BTreeSet<VertexId>,
    edge_ids: BTreeSet<EdgeId>,
    adj_out: BTreeMap<VertexId, BTreeSet<EdgeId>>,
    adj_in: BTreeMap<VertexId, BTreeSet<EdgeId>>,
    label_index: BTreeMap<String, BTreeSet<EdgeId>>,
    vertex_label_index: BTreeMap<String, BTreeSet<VertexId>>,
}

impl Partition {
    fn add_vertex(&mut self, vertex: &Vertex) {
        self.vertex_ids.insert(vertex.vertex_id);
        self.vertex_label_index
            .entry(vertex.label.clone())
            .or_default()
            .insert(vertex.vertex_id);
    }

    fn add_edge(&mut self, edge: &Edge) {
        self.edge_ids.insert(edge.edge_id);
        self.adj_out
            .entry(edge.source_id)
            .or_default()
            .insert(edge.edge_id);
        self.adj_in
            .entry(edge.target_id)
            .or_default()
            .insert(edge.edge_id);
        self.label_index
            .entry(edge.label.clone())
            .or_default()
            .insert(edge.edge_id);
    }

    fn remove_edge(&mut self, edge: &Edge) {
        self.edge_ids.remove(&edge.edge_id);
        if let Some(set) = self.adj_out.get_mut(&edge.source_id) {
            set.remove(&edge.edge_id);
        }
        if let Some(set) = self.adj_in.get_mut(&edge.target_id) {
            set.remove(&edge.edge_id);
        }
        if let Some(set) = self.label_index.get_mut(&edge.label) {
            set.remove(&edge.edge_id);
        }
    }
}

#[derive(Debug, Default)]
pub struct MemoryGraphStore {
    vertices: BTreeMap<VertexId, Vertex>,
    edges: BTreeMap<EdgeId, Edge>,
    graphs: BTreeMap<String, Partition>,
    vertex_membership: BTreeMap<VertexId, BTreeSet<String>>,
    edge_membership: BTreeMap<EdgeId, BTreeSet<String>>,
    next_vertex_id: VertexId,
    next_edge_id: EdgeId,
}

impl MemoryGraphStore {
    pub fn new() -> Self {
        Self {
            next_vertex_id: 1,
            next_edge_id: 1,
            ..Self::default()
        }
    }

    fn require_partition_mut(&mut self, name: &str) -> &mut Partition {
        self.graphs
            .get_mut(name)
            .expect("graph does not exist (call create_graph first)")
    }

    fn require_partition(&self, name: &str) -> &Partition {
        self.graphs
            .get(name)
            .expect("graph does not exist (call create_graph first)")
    }

    fn ensure_partition(&mut self, name: &str) {
        if !self.graphs.contains_key(name) {
            self.graphs.insert(name.to_string(), Partition::default());
        }
    }

    fn release_vertex_if_orphan(&mut self, vertex_id: VertexId) {
        let still_referenced = self
            .vertex_membership
            .get(&vertex_id)
            .is_some_and(|set| !set.is_empty());
        if !still_referenced {
            self.vertices.remove(&vertex_id);
            self.vertex_membership.remove(&vertex_id);
        }
    }

    fn release_edge_if_orphan(&mut self, edge_id: EdgeId) {
        let still_referenced = self
            .edge_membership
            .get(&edge_id)
            .is_some_and(|set| !set.is_empty());
        if !still_referenced {
            self.edges.remove(&edge_id);
            self.edge_membership.remove(&edge_id);
        }
    }

    /// Insert a vertex into the global registry without attaching it
    /// to any graph. Used by the `SQLite`-backed store on hydration
    /// before membership is restored from the persisted catalog.
    pub fn insert_raw_vertex(&mut self, vertex: Vertex) {
        if vertex.vertex_id >= self.next_vertex_id {
            self.next_vertex_id = vertex.vertex_id + 1;
        }
        self.vertices.insert(vertex.vertex_id, vertex);
    }

    pub fn insert_raw_edge(&mut self, edge: Edge) {
        if edge.edge_id >= self.next_edge_id {
            self.next_edge_id = edge.edge_id + 1;
        }
        self.edges.insert(edge.edge_id, edge);
    }

    /// Attach a previously inserted vertex to the named graph. The
    /// graph must already exist (`create_graph`).
    pub fn attach_vertex(&mut self, vertex_id: VertexId, graph: &str) {
        self.ensure_partition(graph);
        let part = self.require_partition_mut(graph);
        part.vertex_ids.insert(vertex_id);
        self.vertex_membership
            .entry(vertex_id)
            .or_default()
            .insert(graph.to_string());
    }

    pub fn attach_edge(&mut self, edge_id: EdgeId, graph: &str) {
        self.ensure_partition(graph);
        // Snapshot the edge fields so we can populate the partition's
        // adjacency indexes without re-borrowing `self.edges`.
        let edge_info = self
            .edges
            .get(&edge_id)
            .map(|e| (e.source_id, e.target_id, e.label.clone()));
        let part = self.require_partition_mut(graph);
        part.edge_ids.insert(edge_id);
        if let Some((src, tgt, label)) = edge_info {
            part.adj_out.entry(src).or_default().insert(edge_id);
            part.adj_in.entry(tgt).or_default().insert(edge_id);
            part.label_index.entry(label).or_default().insert(edge_id);
        }
        self.edge_membership
            .entry(edge_id)
            .or_default()
            .insert(graph.to_string());
    }

    /// All edge ids that participate in `graph`, in stable id order.
    /// Helper for the `SQLite`-backed store's bulk write paths.
    pub fn out_edge_ids_for_graph(&self, graph: &str) -> std::collections::BTreeSet<EdgeId> {
        match self.graphs.get(graph) {
            Some(part) => part.edge_ids.clone(),
            None => std::collections::BTreeSet::new(),
        }
    }
}

impl GraphStore for MemoryGraphStore {
    fn create_graph(&mut self, name: &str) {
        self.graphs.entry(name.to_string()).or_default();
    }

    fn drop_graph(&mut self, name: &str) {
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

    fn graph_names(&self) -> Vec<String> {
        self.graphs.keys().cloned().collect()
    }

    fn has_graph(&self, name: &str) -> bool {
        self.graphs.contains_key(name)
    }

    fn union_graphs(&mut self, g1: &str, g2: &str, target: &str) {
        let v_union: BTreeSet<VertexId> = self
            .require_partition(g1)
            .vertex_ids
            .union(&self.require_partition(g2).vertex_ids)
            .copied()
            .collect();
        let e_union: BTreeSet<EdgeId> = self
            .require_partition(g1)
            .edge_ids
            .union(&self.require_partition(g2).edge_ids)
            .copied()
            .collect();
        self.ensure_partition(target);
        for vid in v_union {
            if let Some(vertex) = self.vertices.get(&vid).cloned() {
                self.require_partition_mut(target).add_vertex(&vertex);
                self.vertex_membership
                    .entry(vid)
                    .or_default()
                    .insert(target.to_string());
            }
        }
        for eid in e_union {
            if let Some(edge) = self.edges.get(&eid).cloned() {
                self.require_partition_mut(target).add_edge(&edge);
                self.edge_membership
                    .entry(eid)
                    .or_default()
                    .insert(target.to_string());
            }
        }
    }

    fn intersect_graphs(&mut self, g1: &str, g2: &str, target: &str) {
        let v_inter: BTreeSet<VertexId> = self
            .require_partition(g1)
            .vertex_ids
            .intersection(&self.require_partition(g2).vertex_ids)
            .copied()
            .collect();
        let e_inter: BTreeSet<EdgeId> = self
            .require_partition(g1)
            .edge_ids
            .intersection(&self.require_partition(g2).edge_ids)
            .copied()
            .collect();
        self.ensure_partition(target);
        for vid in v_inter {
            if let Some(vertex) = self.vertices.get(&vid).cloned() {
                self.require_partition_mut(target).add_vertex(&vertex);
                self.vertex_membership
                    .entry(vid)
                    .or_default()
                    .insert(target.to_string());
            }
        }
        for eid in e_inter {
            if let Some(edge) = self.edges.get(&eid).cloned() {
                self.require_partition_mut(target).add_edge(&edge);
                self.edge_membership
                    .entry(eid)
                    .or_default()
                    .insert(target.to_string());
            }
        }
    }

    fn difference_graphs(&mut self, g1: &str, g2: &str, target: &str) {
        let v_diff: BTreeSet<VertexId> = self
            .require_partition(g1)
            .vertex_ids
            .difference(&self.require_partition(g2).vertex_ids)
            .copied()
            .collect();
        let e_diff: BTreeSet<EdgeId> = self
            .require_partition(g1)
            .edge_ids
            .difference(&self.require_partition(g2).edge_ids)
            .copied()
            .collect();
        self.ensure_partition(target);
        for vid in v_diff {
            if let Some(vertex) = self.vertices.get(&vid).cloned() {
                self.require_partition_mut(target).add_vertex(&vertex);
                self.vertex_membership
                    .entry(vid)
                    .or_default()
                    .insert(target.to_string());
            }
        }
        for eid in e_diff {
            if let Some(edge) = self.edges.get(&eid).cloned() {
                self.require_partition_mut(target).add_edge(&edge);
                self.edge_membership
                    .entry(eid)
                    .or_default()
                    .insert(target.to_string());
            }
        }
    }

    fn copy_graph(&mut self, source: &str, target: &str) {
        let v_copy: BTreeSet<VertexId> = self.require_partition(source).vertex_ids.clone();
        let e_copy: BTreeSet<EdgeId> = self.require_partition(source).edge_ids.clone();
        self.ensure_partition(target);
        for vid in v_copy {
            if let Some(vertex) = self.vertices.get(&vid).cloned() {
                self.require_partition_mut(target).add_vertex(&vertex);
                self.vertex_membership
                    .entry(vid)
                    .or_default()
                    .insert(target.to_string());
            }
        }
        for eid in e_copy {
            if let Some(edge) = self.edges.get(&eid).cloned() {
                self.require_partition_mut(target).add_edge(&edge);
                self.edge_membership
                    .entry(eid)
                    .or_default()
                    .insert(target.to_string());
            }
        }
    }

    fn add_vertex(&mut self, vertex: Vertex, graph: &str) {
        self.ensure_partition(graph);
        let vid = vertex.vertex_id;
        if vid >= self.next_vertex_id {
            self.next_vertex_id = vid + 1;
        }
        self.vertices.insert(vid, vertex.clone());
        self.require_partition_mut(graph).add_vertex(&vertex);
        self.vertex_membership
            .entry(vid)
            .or_default()
            .insert(graph.to_string());
    }

    fn add_edge(&mut self, edge: Edge, graph: &str) {
        self.ensure_partition(graph);
        let eid = edge.edge_id;
        if eid >= self.next_edge_id {
            self.next_edge_id = eid + 1;
        }
        self.edges.insert(eid, edge.clone());
        self.require_partition_mut(graph).add_edge(&edge);
        self.edge_membership
            .entry(eid)
            .or_default()
            .insert(graph.to_string());
    }

    fn remove_vertex(&mut self, vertex_id: VertexId, graph: &str) {
        let Some(partition) = self.graphs.get_mut(graph) else {
            return;
        };
        if !partition.vertex_ids.remove(&vertex_id) {
            return;
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
    }

    fn remove_edge(&mut self, edge_id: EdgeId, graph: &str) {
        let Some(edge) = self.edges.get(&edge_id).cloned() else {
            return;
        };
        let Some(partition) = self.graphs.get_mut(graph) else {
            return;
        };
        if !partition.edge_ids.contains(&edge_id) {
            return;
        }
        partition.remove_edge(&edge);
        if let Some(set) = self.edge_membership.get_mut(&edge_id) {
            set.remove(graph);
        }
        self.release_edge_if_orphan(edge_id);
    }

    fn neighbors(
        &self,
        vertex_id: VertexId,
        label: Option<&str>,
        direction: Direction,
        graph: &str,
    ) -> Vec<VertexId> {
        let partition = self.require_partition(graph);
        let mut result = Vec::new();
        let collect = |set: &BTreeSet<EdgeId>, take_target: bool, out: &mut Vec<VertexId>| {
            for eid in set {
                let Some(edge) = self.edges.get(eid) else {
                    continue;
                };
                if let Some(want) = label {
                    if edge.label != want {
                        continue;
                    }
                }
                out.push(if take_target {
                    edge.target_id
                } else {
                    edge.source_id
                });
            }
        };
        match direction {
            Direction::Out => {
                if let Some(set) = partition.adj_out.get(&vertex_id) {
                    collect(set, true, &mut result);
                }
            }
            Direction::In => {
                if let Some(set) = partition.adj_in.get(&vertex_id) {
                    collect(set, false, &mut result);
                }
            }
            Direction::Both => {
                if let Some(set) = partition.adj_out.get(&vertex_id) {
                    collect(set, true, &mut result);
                }
                if let Some(set) = partition.adj_in.get(&vertex_id) {
                    collect(set, false, &mut result);
                }
                result.sort_unstable();
                result.dedup();
            }
        }
        result
    }

    fn vertices_by_label(&self, label: &str, graph: &str) -> Vec<Vertex> {
        let partition = self.require_partition(graph);
        partition
            .vertex_label_index
            .get(label)
            .into_iter()
            .flat_map(|set| set.iter())
            .filter_map(|vid| self.vertices.get(vid).cloned())
            .collect()
    }

    fn vertices_in_graph(&self, graph: &str) -> Vec<Vertex> {
        self.require_partition(graph)
            .vertex_ids
            .iter()
            .filter_map(|vid| self.vertices.get(vid).cloned())
            .collect()
    }

    fn edges_in_graph(&self, graph: &str) -> Vec<Edge> {
        self.require_partition(graph)
            .edge_ids
            .iter()
            .filter_map(|eid| self.edges.get(eid).cloned())
            .collect()
    }

    fn vertex_graphs(&self, vertex_id: VertexId) -> BTreeSet<String> {
        self.vertex_membership
            .get(&vertex_id)
            .cloned()
            .unwrap_or_default()
    }

    fn out_edge_ids(&self, vertex_id: VertexId, graph: &str) -> BTreeSet<EdgeId> {
        self.require_partition(graph)
            .adj_out
            .get(&vertex_id)
            .cloned()
            .unwrap_or_default()
    }

    fn in_edge_ids(&self, vertex_id: VertexId, graph: &str) -> BTreeSet<EdgeId> {
        self.require_partition(graph)
            .adj_in
            .get(&vertex_id)
            .cloned()
            .unwrap_or_default()
    }

    fn edge_ids_by_label(&self, label: &str, graph: &str) -> BTreeSet<EdgeId> {
        self.require_partition(graph)
            .label_index
            .get(label)
            .cloned()
            .unwrap_or_default()
    }

    fn vertex_ids_in_graph(&self, graph: &str) -> BTreeSet<VertexId> {
        self.require_partition(graph).vertex_ids.clone()
    }

    fn degree_distribution(&self, graph: &str) -> BTreeMap<VertexId, u64> {
        let partition = self.require_partition(graph);
        let mut out = BTreeMap::new();
        for vid in &partition.vertex_ids {
            let degree = partition.adj_out.get(vid).map_or(0, BTreeSet::len) as u64;
            out.insert(*vid, degree);
        }
        out
    }

    fn label_degree(&self, label: &str, graph: &str) -> f64 {
        let partition = self.require_partition(graph);
        let Some(eids) = partition.label_index.get(label) else {
            return 0.0;
        };
        if eids.is_empty() {
            return 0.0;
        }
        let mut sources: BTreeSet<VertexId> = BTreeSet::new();
        for eid in eids {
            if let Some(edge) = self.edges.get(eid) {
                sources.insert(edge.source_id);
            }
        }
        if sources.is_empty() {
            0.0
        } else {
            eids.len() as f64 / sources.len() as f64
        }
    }

    fn vertex_label_counts(&self, graph: &str) -> BTreeMap<String, u64> {
        let partition = self.require_partition(graph);
        let mut out = BTreeMap::new();
        for (label, vids) in &partition.vertex_label_index {
            let count = vids
                .iter()
                .filter(|vid| partition.vertex_ids.contains(vid))
                .count() as u64;
            if count > 0 {
                out.insert(label.clone(), count);
            }
        }
        out
    }

    fn get_vertex(&self, vertex_id: VertexId) -> Option<&Vertex> {
        self.vertices.get(&vertex_id)
    }

    fn get_edge(&self, edge_id: EdgeId) -> Option<&Edge> {
        self.edges.get(&edge_id)
    }

    fn next_vertex_id(&mut self) -> VertexId {
        let id = self.next_vertex_id;
        self.next_vertex_id += 1;
        id
    }

    fn next_edge_id(&mut self) -> EdgeId {
        let id = self.next_edge_id;
        self.next_edge_id += 1;
        id
    }

    fn clear(&mut self) {
        self.vertices.clear();
        self.edges.clear();
        self.graphs.clear();
        self.vertex_membership.clear();
        self.edge_membership.clear();
        self.next_vertex_id = 1;
        self.next_edge_id = 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_basic_graph() -> MemoryGraphStore {
        let mut store = MemoryGraphStore::new();
        store.create_graph("g");
        store.add_vertex(Vertex::new(1, "person"), "g");
        store.add_vertex(Vertex::new(2, "person"), "g");
        store.add_vertex(Vertex::new(3, "company"), "g");
        store.add_edge(Edge::new(10, 1, 2, "knows"), "g");
        store.add_edge(Edge::new(11, 1, 3, "works_at"), "g");
        store
    }

    #[test]
    fn neighbors_filter_by_label() {
        let store = build_basic_graph();
        let mut out = store.neighbors(1, Some("knows"), Direction::Out, "g");
        out.sort_unstable();
        assert_eq!(out, vec![2]);
    }

    #[test]
    fn neighbors_in_direction() {
        let store = build_basic_graph();
        let inn = store.neighbors(2, None, Direction::In, "g");
        assert_eq!(inn, vec![1]);
    }

    #[test]
    fn neighbors_both_dedupes() {
        let mut store = build_basic_graph();
        // Self-loop the other way.
        store.add_edge(Edge::new(12, 2, 1, "knows"), "g");
        let mut out = store.neighbors(1, Some("knows"), Direction::Both, "g");
        out.sort_unstable();
        assert_eq!(out, vec![2]);
    }

    #[test]
    fn drop_graph_releases_orphan_records() {
        let mut store = build_basic_graph();
        store.drop_graph("g");
        assert!(store.get_vertex(1).is_none());
        assert!(store.get_edge(10).is_none());
        assert!(!store.has_graph("g"));
    }

    #[test]
    fn membership_tracks_multiple_graphs() {
        let mut store = MemoryGraphStore::new();
        store.create_graph("a");
        store.create_graph("b");
        store.add_vertex(Vertex::new(1, "node"), "a");
        store.add_vertex(Vertex::new(1, "node"), "b");
        let mship = store.vertex_graphs(1);
        assert!(mship.contains("a") && mship.contains("b"));
        store.drop_graph("a");
        // Vertex 1 still belongs to "b", so it must survive.
        assert!(store.get_vertex(1).is_some());
        assert_eq!(
            store.vertex_graphs(1).into_iter().collect::<Vec<_>>(),
            vec!["b".to_string()]
        );
    }

    #[test]
    fn graph_algebra_union_intersect_difference() {
        let mut store = MemoryGraphStore::new();
        store.create_graph("g1");
        store.create_graph("g2");
        store.add_vertex(Vertex::new(1, "v"), "g1");
        store.add_vertex(Vertex::new(2, "v"), "g1");
        store.add_vertex(Vertex::new(2, "v"), "g2");
        store.add_vertex(Vertex::new(3, "v"), "g2");

        store.union_graphs("g1", "g2", "u");
        let u_ids: Vec<_> = store.vertex_ids_in_graph("u").into_iter().collect();
        assert_eq!(u_ids, vec![1, 2, 3]);

        store.intersect_graphs("g1", "g2", "i");
        let i_ids: Vec<_> = store.vertex_ids_in_graph("i").into_iter().collect();
        assert_eq!(i_ids, vec![2]);

        store.difference_graphs("g1", "g2", "d");
        let d_ids: Vec<_> = store.vertex_ids_in_graph("d").into_iter().collect();
        assert_eq!(d_ids, vec![1]);
    }

    #[test]
    fn next_id_advances() {
        let mut store = MemoryGraphStore::new();
        assert_eq!(store.next_vertex_id(), 1);
        assert_eq!(store.next_vertex_id(), 2);
        assert_eq!(store.next_edge_id(), 1);
        store.add_vertex(Vertex::new(99, "v"), "g");
        assert_eq!(store.next_vertex_id(), 100);
    }
}
