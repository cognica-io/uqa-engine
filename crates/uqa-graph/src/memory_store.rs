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

use crate::store::{GraphStore, GraphStoreError, GraphStoreResult};
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

/// The largest label id whose AGE graphid remains representable as a signed
/// 64-bit agtype integer.
pub const MAX_GRAPHID_LABEL_ID: u32 = 32_767;

const MAX_GRAPHID_SEQUENCE: u64 = (1_u64 << GRAPHID_LABEL_SHIFT) - 1;
const MAX_EXACT_F64_INTEGER: u64 = 9_007_199_254_740_992;

fn usize_to_f64_exact(value: usize, context: &str) -> GraphStoreResult<f64> {
    if u64::try_from(value).is_ok_and(|value| value <= MAX_EXACT_F64_INTEGER) {
        Ok(value as f64)
    } else {
        Err(GraphStoreError::InvalidMutation(format!(
            "{context} {value} exceeds the exact f64 integer range"
        )))
    }
}

/// Compose an AGE `graphid` from a label id and per-label sequence.
pub fn make_graphid(label_id: u32, sequence: u64) -> GraphStoreResult<u64> {
    if label_id > MAX_GRAPHID_LABEL_ID {
        return Err(GraphStoreError::IdExhausted(format!(
            "label id {label_id} exceeds {MAX_GRAPHID_LABEL_ID}"
        )));
    }
    if sequence == 0 || sequence > MAX_GRAPHID_SEQUENCE {
        return Err(GraphStoreError::IdExhausted(format!(
            "sequence {sequence} is outside 1..={MAX_GRAPHID_SEQUENCE}"
        )));
    }
    Ok((u64::from(label_id) << GRAPHID_LABEL_SHIFT) | sequence)
}

/// Label id component of an AGE `graphid`.
#[must_use]
pub fn graphid_label_id(id: u64) -> u32 {
    let bytes = id.to_be_bytes();
    u32::from(u16::from_be_bytes([bytes[0], bytes[1]]))
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
#[serde(default)]
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
    fn label_id(&mut self, label: &str, default_id: u32) -> GraphStoreResult<u32> {
        if label.is_empty() {
            return Ok(default_id);
        }
        if let Some(id) = self.labels.get(label) {
            if *id > MAX_GRAPHID_LABEL_ID {
                return Err(GraphStoreError::IdExhausted(format!(
                    "persisted label id {id} exceeds {MAX_GRAPHID_LABEL_ID}"
                )));
            }
            return Ok(*id);
        }
        let id = self.next_label_id;
        if id > MAX_GRAPHID_LABEL_ID {
            return Err(GraphStoreError::IdExhausted(format!(
                "label id {id} exceeds {MAX_GRAPHID_LABEL_ID}"
            )));
        }
        self.next_label_id = id
            .checked_add(1)
            .ok_or_else(|| GraphStoreError::IdExhausted("label id counter overflow".to_string()))?;
        self.labels.insert(label.to_string(), id);
        Ok(id)
    }

    fn next_sequence(&mut self, label_id: u32) -> GraphStoreResult<u64> {
        let current = self.sequences.get(&label_id).copied().unwrap_or(0);
        let next = current.checked_add(1).ok_or_else(|| {
            GraphStoreError::IdExhausted(format!(
                "sequence counter overflow for label id {label_id}"
            ))
        })?;
        if next > MAX_GRAPHID_SEQUENCE {
            return Err(GraphStoreError::IdExhausted(format!(
                "sequence {next} exceeds {MAX_GRAPHID_SEQUENCE} for label id {label_id}"
            )));
        }
        self.sequences.insert(label_id, next);
        Ok(next)
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

#[derive(Debug, Default, Clone)]
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

#[derive(Debug, Default, Clone)]
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

    #[cfg(test)]
    pub(crate) fn remove_edge_record_for_corruption_test(&mut self, edge_id: EdgeId) {
        self.edges.remove(&edge_id);
    }

    #[cfg(test)]
    fn remove_vertex_record_for_corruption_test(&mut self, vertex_id: VertexId) {
        self.vertices.remove(&vertex_id);
    }

    fn require_partition_mut(&mut self, name: &str) -> GraphStoreResult<&mut Partition> {
        self.graphs
            .get_mut(name)
            .ok_or_else(|| GraphStoreError::UnknownGraph(name.to_string()))
    }

    fn require_partition(&self, name: &str) -> GraphStoreResult<&Partition> {
        self.graphs
            .get(name)
            .ok_or_else(|| GraphStoreError::UnknownGraph(name.to_string()))
    }

    fn require_query_vertex(
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

    fn require_partition_vertex<'a>(
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

    fn require_partition_edge<'a>(
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

    fn populate_graph_from_ids(
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
        self.vertices.insert(vid, vertex.clone());
        self.require_partition_mut(graph)?.add_vertex(&vertex);
        self.vertex_membership
            .entry(vid)
            .or_default()
            .insert(graph.to_string());
        self.next_vertex_id = next_vertex_id;
        Ok(())
    }

    fn add_edge(&mut self, edge: Edge, graph: &str) -> GraphStoreResult<()> {
        let partition = self.require_partition(graph)?;
        if !partition.vertex_ids.contains(&edge.source_id)
            || !partition.vertex_ids.contains(&edge.target_id)
        {
            return Err(GraphStoreError::InvalidMutation(format!(
                "edge {} references endpoint outside graph {graph:?}: {} -> {}",
                edge.edge_id, edge.source_id, edge.target_id
            )));
        }
        self.require_partition_vertex(partition, edge.source_id, graph)?;
        self.require_partition_vertex(partition, edge.target_id, graph)?;
        let eid = edge.edge_id;
        let next_edge_id = if eid >= self.next_edge_id {
            eid.checked_add(1).ok_or_else(|| {
                GraphStoreError::IdExhausted("edge id counter overflow".to_string())
            })?
        } else {
            self.next_edge_id
        };
        self.edges.insert(eid, edge.clone());
        self.require_partition_mut(graph)?.add_edge(&edge);
        self.edge_membership
            .entry(eid)
            .or_default()
            .insert(graph.to_string());
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

    fn vertex_graphs(&self, vertex_id: VertexId) -> BTreeSet<String> {
        self.vertex_membership
            .get(&vertex_id)
            .cloned()
            .unwrap_or_default()
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

    fn get_vertex(&self, vertex_id: VertexId) -> Option<&Vertex> {
        self.vertices.get(&vertex_id)
    }

    fn get_edge(&self, edge_id: EdgeId) -> Option<&Edge> {
        self.edges.get(&edge_id)
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
        let label_id = candidate.label_id(label, VERTEX_DEFAULT_LABEL_ID)?;
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
        let label_id = candidate.label_id(label, EDGE_DEFAULT_LABEL_ID)?;
        let id = make_graphid(label_id, candidate.next_sequence(label_id)?)?;
        self.label_registries.insert(graph.to_string(), candidate);
        Ok(id)
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
        store.add_vertex(Vertex::new(1, "person"), "g").unwrap();
        store.add_vertex(Vertex::new(2, "person"), "g").unwrap();
        store.add_vertex(Vertex::new(3, "company"), "g").unwrap();
        store.add_edge(Edge::new(10, 1, 2, "knows"), "g").unwrap();
        store
            .add_edge(Edge::new(11, 1, 3, "works_at"), "g")
            .unwrap();
        store
    }

    #[test]
    fn neighbors_filter_by_label() {
        let store = build_basic_graph();
        let mut out = store
            .neighbors(1, Some("knows"), Direction::Out, "g")
            .unwrap();
        out.sort_unstable();
        assert_eq!(out, vec![2]);
    }

    #[test]
    fn missing_query_vertex_is_not_an_empty_neighborhood() {
        let store = build_basic_graph();
        for result in [
            store.neighbors(999, None, Direction::Out, "g").map(|_| ()),
            store.out_edge_ids(999, "g").map(|_| ()),
            store.in_edge_ids(999, "g").map(|_| ()),
        ] {
            assert!(matches!(result, Err(GraphStoreError::InvalidQuery(_))));
        }
    }

    #[test]
    fn edge_endpoints_must_belong_to_the_target_graph() {
        let mut store = MemoryGraphStore::new();
        store.create_graph("left");
        store.create_graph("right");
        store.add_vertex(Vertex::new(1, "v"), "left").unwrap();
        store.add_vertex(Vertex::new(2, "v"), "right").unwrap();

        assert!(matches!(
            store.add_edge(Edge::new(10, 1, 2, "cross"), "left"),
            Err(GraphStoreError::InvalidMutation(message))
                if message.contains("outside graph")
        ));
    }

    #[test]
    fn dangling_membership_records_surface_as_corruption() {
        let mut missing_edge = build_basic_graph();
        missing_edge.remove_edge_record_for_corruption_test(10);
        assert!(matches!(
            missing_edge.neighbors(1, None, Direction::Out, "g"),
            Err(GraphStoreError::CorruptGraph(_))
        ));
        assert!(matches!(
            missing_edge.edges_in_graph("g"),
            Err(GraphStoreError::CorruptGraph(_))
        ));

        let mut missing_vertex = build_basic_graph();
        missing_vertex.remove_vertex_record_for_corruption_test(2);
        assert!(matches!(
            missing_vertex.vertices_in_graph("g"),
            Err(GraphStoreError::CorruptGraph(_))
        ));
        assert!(matches!(
            missing_vertex.vertex_ids_by_label("person", "g"),
            Err(GraphStoreError::CorruptGraph(_))
        ));
    }

    #[test]
    fn vertex_ids_by_label_uses_label_membership() {
        let store = build_basic_graph();
        assert_eq!(
            store.vertex_ids_by_label("person", "g").unwrap(),
            vec![1, 2]
        );
        assert_eq!(store.vertex_ids_by_label("company", "g").unwrap(), vec![3]);
        assert!(store
            .vertex_ids_by_label("missing", "g")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn neighbors_in_direction() {
        let store = build_basic_graph();
        let inn = store.neighbors(2, None, Direction::In, "g").unwrap();
        assert_eq!(inn, vec![1]);
    }

    #[test]
    fn neighbors_both_dedupes() {
        let mut store = build_basic_graph();
        // Self-loop the other way.
        store.add_edge(Edge::new(12, 2, 1, "knows"), "g").unwrap();
        let mut out = store
            .neighbors(1, Some("knows"), Direction::Both, "g")
            .unwrap();
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
        store.add_vertex(Vertex::new(1, "node"), "a").unwrap();
        store.add_vertex(Vertex::new(1, "node"), "b").unwrap();
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
        store.add_vertex(Vertex::new(1, "v"), "g1").unwrap();
        store.add_vertex(Vertex::new(2, "v"), "g1").unwrap();
        store.add_vertex(Vertex::new(2, "v"), "g2").unwrap();
        store.add_vertex(Vertex::new(3, "v"), "g2").unwrap();

        store.union_graphs("g1", "g2", "u").unwrap();
        let u_ids: Vec<_> = store
            .vertex_ids_in_graph("u")
            .unwrap()
            .into_iter()
            .collect();
        assert_eq!(u_ids, vec![1, 2, 3]);

        store.intersect_graphs("g1", "g2", "i").unwrap();
        let i_ids: Vec<_> = store
            .vertex_ids_in_graph("i")
            .unwrap()
            .into_iter()
            .collect();
        assert_eq!(i_ids, vec![2]);

        store.difference_graphs("g1", "g2", "d").unwrap();
        let d_ids: Vec<_> = store
            .vertex_ids_in_graph("d")
            .unwrap()
            .into_iter()
            .collect();
        assert_eq!(d_ids, vec![1]);
    }

    #[test]
    fn next_id_advances() {
        let mut store = MemoryGraphStore::new();
        store.create_graph("g");
        assert_eq!(store.next_vertex_id().unwrap(), 1);
        assert_eq!(store.next_vertex_id().unwrap(), 2);
        assert_eq!(store.next_edge_id().unwrap(), 1);
        store.add_vertex(Vertex::new(99, "v"), "g").unwrap();
        assert_eq!(store.next_vertex_id().unwrap(), 100);
    }

    #[test]
    fn allocate_ids_follow_age_graphid_scheme() {
        let mut store = MemoryGraphStore::new();
        store.create_graph("g");
        // First user vertex label -> label id 3, sequence 1.
        assert_eq!(
            store.allocate_vertex_id("Person", "g").unwrap(),
            844_424_930_131_969
        );
        assert_eq!(
            store.allocate_vertex_id("Person", "g").unwrap(),
            844_424_930_131_970
        );
        // Edge labels share the same per-graph label counter -> 4.
        assert_eq!(
            store.allocate_edge_id("KNOWS", "g").unwrap(),
            1_125_899_906_842_625
        );
        // Next new vertex label continues the shared counter -> 5.
        assert_eq!(
            store.allocate_vertex_id("City", "g").unwrap(),
            1_407_374_883_553_281
        );
        // Unlabeled entities land in the reserved label ids 1 / 2.
        assert_eq!(
            store.allocate_vertex_id("", "g").unwrap(),
            make_graphid(1, 1).unwrap()
        );
        assert_eq!(
            store.allocate_edge_id("", "g").unwrap(),
            make_graphid(2, 1).unwrap()
        );
        // Sequences are per label.
        assert_eq!(
            store.allocate_edge_id("KNOWS", "g").unwrap(),
            make_graphid(4, 2).unwrap()
        );
    }

    #[test]
    fn label_registry_rebuild_from_ids_self_heals() {
        let mut store = MemoryGraphStore::new();
        store.create_graph("g");
        store
            .add_vertex(Vertex::new(make_graphid(3, 7).unwrap(), "Person"), "g")
            .unwrap();
        store
            .add_edge(
                Edge::new(
                    make_graphid(4, 2).unwrap(),
                    make_graphid(3, 7).unwrap(),
                    make_graphid(3, 7).unwrap(),
                    "KNOWS",
                ),
                "g",
            )
            .unwrap();
        store.rebuild_label_registry_from_ids("g");
        // New allocations continue after the observed watermarks.
        assert_eq!(
            store.allocate_vertex_id("Person", "g").unwrap(),
            make_graphid(3, 8).unwrap()
        );
        assert_eq!(
            store.allocate_edge_id("KNOWS", "g").unwrap(),
            make_graphid(4, 3).unwrap()
        );
        // A brand-new label picks the next free label id (5).
        assert_eq!(
            store.allocate_vertex_id("City", "g").unwrap(),
            make_graphid(5, 1).unwrap()
        );
    }

    #[test]
    fn label_registry_survives_round_trip_via_import() {
        let mut store = MemoryGraphStore::new();
        store.create_graph("g");
        store.allocate_vertex_id("Person", "g").unwrap();
        let registry = store.label_registry("g");
        let json = serde_json::to_string(&registry).unwrap();
        let restored: GraphLabelRegistry = serde_json::from_str(&json).unwrap();

        let mut fresh = MemoryGraphStore::new();
        fresh.create_graph("g");
        fresh.import_label_registry("g", &restored);
        assert_eq!(
            fresh.allocate_vertex_id("Person", "g").unwrap(),
            make_graphid(3, 2).unwrap()
        );
    }

    #[test]
    fn drop_graph_resets_label_registry() {
        let mut store = MemoryGraphStore::new();
        store.create_graph("g");
        let first = store.allocate_vertex_id("Person", "g").unwrap();
        store.drop_graph("g");
        store.create_graph("g");
        assert_eq!(store.allocate_vertex_id("Person", "g").unwrap(), first);
    }

    #[test]
    fn graph_id_allocation_rejects_missing_graph_and_exhaustion() {
        let mut store = MemoryGraphStore::new();
        assert!(matches!(
            store.allocate_vertex_id("Person", "missing"),
            Err(GraphStoreError::UnknownGraph(_))
        ));
        assert!(make_graphid(MAX_GRAPHID_LABEL_ID + 1, 1).is_err());
        assert!(make_graphid(1, MAX_GRAPHID_SEQUENCE + 1).is_err());
    }
}
