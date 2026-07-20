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
//!
//! Ids allocated through [`GraphStore::allocate_vertex_id`] /
//! [`GraphStore::allocate_edge_id`] follow the Apache AGE `graphid`
//! scheme: `(label_id << 48) | per_label_sequence`, where label ids 1
//! and 2 are reserved for AGE's internal `_ag_label_vertex` /
//! `_ag_label_edge` (unlabeled entities) and user labels start at 3,
//! sharing one per-graph counter across vertex and edge labels in
//! creation order.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use uqa_core::{Edge, EdgeId, Vertex, VertexId};

use crate::store::GraphStore;
use crate::types::Direction;

/// Number of bits reserved for the per-label sequence inside an AGE
/// `graphid`. The label id occupies the remaining high 16 bits.
pub const GRAPHID_LABEL_SHIFT: u32 = 48;

/// Reserved AGE label id for unlabeled vertices (`_ag_label_vertex`).
pub const VERTEX_DEFAULT_LABEL_ID: u32 = 1;

/// Reserved AGE label id for unlabeled edges (`_ag_label_edge`).
pub const EDGE_DEFAULT_LABEL_ID: u32 = 2;

/// First label id available to user labels.
pub const FIRST_USER_LABEL_ID: u32 = 3;

/// Compose an AGE `graphid` from a label id and per-label sequence.
#[must_use]
pub fn make_graphid(label_id: u32, sequence: u64) -> u64 {
    (u64::from(label_id) << GRAPHID_LABEL_SHIFT) | (sequence & ((1 << GRAPHID_LABEL_SHIFT) - 1))
}

/// Label id component of an AGE `graphid`.
#[must_use]
pub fn graphid_label_id(id: u64) -> u32 {
    (id >> GRAPHID_LABEL_SHIFT) as u32
}

/// Sequence component of an AGE `graphid`.
#[must_use]
pub fn graphid_sequence(id: u64) -> u64 {
    id & ((1 << GRAPHID_LABEL_SHIFT) - 1)
}

/// Per-graph AGE label registry: label name -> label id plus the
/// per-label id sequences. Serializable so engines can persist it in
/// catalog metadata and restore deterministic id allocation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphLabelRegistry {
    /// Label name -> AGE label id. Vertex and edge labels share the
    /// namespace-wide counter; the reserved names for ids 1 / 2 are
    /// not stored here (empty labels map onto them implicitly).
    pub labels: BTreeMap<String, u32>,
    /// Label id -> last allocated per-label sequence value.
    pub sequences: BTreeMap<u32, u64>,
    /// Next label id handed to a previously unseen label.
    pub next_label_id: u32,
}

impl Default for GraphLabelRegistry {
    fn default() -> Self {
        Self {
            labels: BTreeMap::new(),
            sequences: BTreeMap::new(),
            next_label_id: FIRST_USER_LABEL_ID,
        }
    }
}

impl GraphLabelRegistry {
    fn label_id(&mut self, label: &str, default_id: u32) -> u32 {
        if label.is_empty() {
            return default_id;
        }
        if let Some(id) = self.labels.get(label) {
            return *id;
        }
        let id = self.next_label_id;
        self.next_label_id += 1;
        self.labels.insert(label.to_string(), id);
        id
    }

    fn next_sequence(&mut self, label_id: u32) -> u64 {
        let seq = self.sequences.entry(label_id).or_insert(0);
        *seq += 1;
        *seq
    }

    /// Fold an existing entity id back into the registry so restored
    /// graphs never re-issue an id that is already in use.
    fn observe(&mut self, label: &str, id: u64) {
        let label_id = graphid_label_id(id);
        if label_id == 0 {
            // Pre-AGE id (plain counter) - nothing to learn.
            return;
        }
        if !label.is_empty() && label_id >= FIRST_USER_LABEL_ID {
            self.labels.entry(label.to_string()).or_insert(label_id);
        }
        let seq = graphid_sequence(id);
        let entry = self.sequences.entry(label_id).or_insert(0);
        if seq > *entry {
            *entry = seq;
        }
        if label_id >= self.next_label_id {
            self.next_label_id = label_id + 1;
        }
    }

    /// Merge another registry (e.g. persisted metadata) into this one,
    /// keeping the larger sequence values and label id watermark.
    pub fn merge(&mut self, other: &GraphLabelRegistry) {
        for (label, id) in &other.labels {
            self.labels.entry(label.clone()).or_insert(*id);
        }
        for (label_id, seq) in &other.sequences {
            let entry = self.sequences.entry(*label_id).or_insert(0);
            if *seq > *entry {
                *entry = *seq;
            }
        }
        if other.next_label_id > self.next_label_id {
            self.next_label_id = other.next_label_id;
        }
    }
}

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
    label_registries: BTreeMap<String, GraphLabelRegistry>,
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

impl GraphStore for MemoryGraphStore {
    fn create_graph(&mut self, name: &str) {
        self.graphs.entry(name.to_string()).or_default();
    }

    fn drop_graph(&mut self, name: &str) {
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

    fn vertex_ids_by_label(&self, label: &str, graph: &str) -> Vec<VertexId> {
        self.require_partition(graph)
            .vertex_label_index
            .get(label)
            .into_iter()
            .flat_map(|set| set.iter())
            .copied()
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

    fn allocate_vertex_id(&mut self, label: &str, graph: &str) -> VertexId {
        let registry = self.label_registries.entry(graph.to_string()).or_default();
        let label_id = registry.label_id(label, VERTEX_DEFAULT_LABEL_ID);
        make_graphid(label_id, registry.next_sequence(label_id))
    }

    fn allocate_edge_id(&mut self, label: &str, graph: &str) -> EdgeId {
        let registry = self.label_registries.entry(graph.to_string()).or_default();
        let label_id = registry.label_id(label, EDGE_DEFAULT_LABEL_ID);
        make_graphid(label_id, registry.next_sequence(label_id))
    }

    fn clear(&mut self) {
        self.vertices.clear();
        self.edges.clear();
        self.graphs.clear();
        self.vertex_membership.clear();
        self.edge_membership.clear();
        self.label_registries.clear();
        self.next_vertex_id = 1;
        self.next_edge_id = 1;
    }

    fn vertices(&self) -> BTreeMap<VertexId, Vertex> {
        self.vertices.clone()
    }

    fn edges(&self) -> BTreeMap<EdgeId, Edge> {
        self.edges.clone()
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
    fn vertex_ids_by_label_uses_label_membership() {
        let store = build_basic_graph();
        assert_eq!(store.vertex_ids_by_label("person", "g"), vec![1, 2]);
        assert_eq!(store.vertex_ids_by_label("company", "g"), vec![3]);
        assert!(store.vertex_ids_by_label("missing", "g").is_empty());
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

    #[test]
    fn allocate_ids_follow_age_graphid_scheme() {
        let mut store = MemoryGraphStore::new();
        store.create_graph("g");
        // First user vertex label -> label id 3, sequence 1.
        assert_eq!(store.allocate_vertex_id("Person", "g"), 844_424_930_131_969);
        assert_eq!(store.allocate_vertex_id("Person", "g"), 844_424_930_131_970);
        // Edge labels share the same per-graph label counter -> 4.
        assert_eq!(store.allocate_edge_id("KNOWS", "g"), 1_125_899_906_842_625);
        // Next new vertex label continues the shared counter -> 5.
        assert_eq!(store.allocate_vertex_id("City", "g"), 1_407_374_883_553_281);
        // Unlabeled entities land in the reserved label ids 1 / 2.
        assert_eq!(store.allocate_vertex_id("", "g"), make_graphid(1, 1));
        assert_eq!(store.allocate_edge_id("", "g"), make_graphid(2, 1));
        // Sequences are per label.
        assert_eq!(store.allocate_edge_id("KNOWS", "g"), make_graphid(4, 2));
    }

    #[test]
    fn label_registry_rebuild_from_ids_self_heals() {
        let mut store = MemoryGraphStore::new();
        store.create_graph("g");
        store.add_vertex(Vertex::new(make_graphid(3, 7), "Person"), "g");
        store.add_edge(
            Edge::new(
                make_graphid(4, 2),
                make_graphid(3, 7),
                make_graphid(3, 7),
                "KNOWS",
            ),
            "g",
        );
        store.rebuild_label_registry_from_ids("g");
        // New allocations continue after the observed watermarks.
        assert_eq!(store.allocate_vertex_id("Person", "g"), make_graphid(3, 8));
        assert_eq!(store.allocate_edge_id("KNOWS", "g"), make_graphid(4, 3));
        // A brand-new label picks the next free label id (5).
        assert_eq!(store.allocate_vertex_id("City", "g"), make_graphid(5, 1));
    }

    #[test]
    fn label_registry_survives_round_trip_via_import() {
        let mut store = MemoryGraphStore::new();
        store.create_graph("g");
        let _ = store.allocate_vertex_id("Person", "g");
        let registry = store.label_registry("g");
        let json = serde_json::to_string(&registry).unwrap();
        let restored: GraphLabelRegistry = serde_json::from_str(&json).unwrap();

        let mut fresh = MemoryGraphStore::new();
        fresh.create_graph("g");
        fresh.import_label_registry("g", &restored);
        assert_eq!(fresh.allocate_vertex_id("Person", "g"), make_graphid(3, 2));
    }

    #[test]
    fn drop_graph_resets_label_registry() {
        let mut store = MemoryGraphStore::new();
        store.create_graph("g");
        let first = store.allocate_vertex_id("Person", "g");
        store.drop_graph("g");
        store.create_graph("g");
        assert_eq!(store.allocate_vertex_id("Person", "g"), first);
    }
}
