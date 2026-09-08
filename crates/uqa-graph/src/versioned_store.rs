//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Version-tracked graph store (Section 9.3, Paper 2).
//!
//! Wraps a [`GraphStore`] and applies [`GraphDelta`]s with a monotonic
//! version counter. Each apply records an inverse delta so the store
//! can rewind to an earlier version. Invalidation callbacks fire on
//! affected edge labels so dependent path indexes can refresh.

use std::collections::BTreeSet;

use crate::delta::{DeltaOp, GraphDelta};
use crate::store::{GraphStore, GraphStoreError, GraphStoreResult};

type InvalidationCallback = Box<dyn Fn(&BTreeSet<String>) + Send + Sync>;
type InverseDelta = Vec<(String, DeltaOp)>;

fn record_vertex_removal<G: GraphStore>(
    candidate: &mut G,
    graph: &str,
    vertex_id: u64,
    inverse: &mut InverseDelta,
    affected_labels: &mut BTreeSet<String>,
) -> GraphStoreResult<()> {
    if !candidate
        .vertex_graphs(vertex_id)?
        .iter()
        .any(|owner| owner == graph)
    {
        return Ok(());
    }
    let existing = candidate
        .get_vertex(vertex_id)?
        .ok_or_else(|| GraphStoreError::CorruptGraph(format!("missing vertex {vertex_id}")))?;
    let mut incident_edges = Vec::new();
    let mut ids = candidate.out_edge_ids(vertex_id, graph)?;
    ids.extend(candidate.in_edge_ids(vertex_id, graph)?);
    for id in ids {
        let edge = candidate
            .get_edge(id)?
            .ok_or_else(|| GraphStoreError::CorruptGraph(format!("missing incident edge {id}")))?;
        affected_labels.insert(edge.label.clone());
        incident_edges.push(edge);
    }
    candidate.remove_vertex(vertex_id, graph)?;
    for edge in incident_edges {
        inverse.push((graph.to_string(), DeltaOp::AddEdge(edge)));
    }
    inverse.push((graph.to_string(), DeltaOp::AddVertex(existing)));
    Ok(())
}

pub struct VersionedGraphStore<'a, G: GraphStore> {
    base: &'a mut G,
    graph: String,
    version: u64,
    deltas: Vec<GraphDelta>,
    inverse_deltas: Vec<InverseDelta>,
    on_invalidate: Vec<InvalidationCallback>,
}

impl<'a, G: GraphStore> VersionedGraphStore<'a, G> {
    pub fn new(base: &'a mut G, graph: impl Into<String>) -> Self {
        Self {
            base,
            graph: graph.into(),
            version: 0,
            deltas: Vec::new(),
            inverse_deltas: Vec::new(),
            on_invalidate: Vec::new(),
        }
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn base(&self) -> &G {
        &*self.base
    }

    pub fn base_mut(&mut self) -> &mut G {
        self.base
    }

    /// Apply a delta to the base store, accumulating an inverse delta
    /// for rollback. Returns the new version number.
    pub fn apply(&mut self, delta: GraphDelta) -> GraphStoreResult<u64> {
        if !self.base.has_graph(&self.graph)? {
            return Err(GraphStoreError::UnknownGraph(self.graph.clone()));
        }
        let next_version = self.version.checked_add(1).ok_or_else(|| {
            GraphStoreError::IdExhausted("graph version counter overflow".to_string())
        })?;
        let mut inverse = InverseDelta::new();
        let mut affected_labels = delta.affected_edge_labels();
        self.base.transaction(|candidate| {
            for op in delta.ops() {
                match op {
                    DeltaOp::AddVertex(vertex) => {
                        if let Some(previous) = candidate.get_vertex(vertex.vertex_id)? {
                            let owners = candidate.vertex_graphs(vertex.vertex_id)?;
                            let owner = owners.first().ok_or_else(|| {
                                GraphStoreError::CorruptGraph(format!(
                                    "vertex {} has no owning graph",
                                    vertex.vertex_id
                                ))
                            })?;
                            inverse.push((owner.clone(), DeltaOp::AddVertex(previous)));
                            if !owners.contains(&self.graph) {
                                inverse.push((
                                    self.graph.clone(),
                                    DeltaOp::RemoveVertex(vertex.vertex_id),
                                ));
                            }
                        } else {
                            inverse.push((
                                self.graph.clone(),
                                DeltaOp::RemoveVertex(vertex.vertex_id),
                            ));
                        }
                        candidate.add_vertex(vertex.clone(), &self.graph)?;
                    }
                    DeltaOp::RemoveVertex(vertex_id) => {
                        record_vertex_removal(
                            candidate,
                            &self.graph,
                            *vertex_id,
                            &mut inverse,
                            &mut affected_labels,
                        )?;
                    }
                    DeltaOp::AddEdge(edge) => {
                        if let Some(previous) = candidate.get_edge(edge.edge_id)? {
                            affected_labels.insert(previous.label.clone());
                            let owners = candidate.edge_graphs(edge.edge_id)?;
                            let owner = owners.first().ok_or_else(|| {
                                GraphStoreError::CorruptGraph(format!(
                                    "edge {} has no owning graph",
                                    edge.edge_id
                                ))
                            })?;
                            inverse.push((owner.clone(), DeltaOp::AddEdge(previous)));
                            if !owners.contains(&self.graph) {
                                inverse
                                    .push((self.graph.clone(), DeltaOp::RemoveEdge(edge.edge_id)));
                            }
                        } else {
                            inverse.push((self.graph.clone(), DeltaOp::RemoveEdge(edge.edge_id)));
                        }
                        candidate.add_edge(edge.clone(), &self.graph)?;
                    }
                    DeltaOp::RemoveEdge(edge_id) => {
                        if !candidate.edge_graphs(*edge_id)?.contains(&self.graph) {
                            continue;
                        }
                        let existing = candidate.get_edge(*edge_id)?.ok_or_else(|| {
                            GraphStoreError::CorruptGraph(format!("missing edge {edge_id}"))
                        })?;
                        affected_labels.insert(existing.label.clone());
                        candidate.remove_edge(*edge_id, &self.graph)?;
                        inverse.push((self.graph.clone(), DeltaOp::AddEdge(existing)));
                    }
                }
            }
            Ok(())
        })?;
        self.version = next_version;
        self.deltas.push(delta);
        self.inverse_deltas.push(inverse);
        if !affected_labels.is_empty() {
            for callback in &self.on_invalidate {
                callback(&affected_labels);
            }
        }
        Ok(self.version)
    }

    /// Rewind to the given version by replaying inverse deltas. Errors
    /// when the target version is in the future or below zero.
    pub fn rollback(&mut self, to_version: u64) -> GraphStoreResult<()> {
        if to_version > self.version {
            return Err(GraphStoreError::InvalidMutation(format!(
                "cannot rollback to version {to_version} (current: {})",
                self.version
            )));
        }
        let mut remaining_version = self.version;
        let mut inverse_count = 0usize;
        self.base.transaction(|candidate| {
            while remaining_version > to_version {
                let offset = inverse_count.checked_add(1).ok_or_else(|| {
                    GraphStoreError::CorruptGraph("version history index overflow".to_string())
                })?;
                let inverse = self
                    .inverse_deltas
                    .get(
                        self.inverse_deltas
                            .len()
                            .checked_sub(offset)
                            .ok_or_else(|| {
                                GraphStoreError::CorruptGraph(
                                    "version history is shorter than the current graph version"
                                        .to_string(),
                                )
                            })?,
                    )
                    .ok_or_else(|| {
                        GraphStoreError::CorruptGraph(
                            "version history is shorter than the current graph version".to_string(),
                        )
                    })?;
                for (graph, op) in inverse.iter().rev() {
                    match op {
                        DeltaOp::AddVertex(vertex) => {
                            candidate.add_vertex(vertex.clone(), graph)?;
                        }
                        DeltaOp::RemoveVertex(vertex_id) => {
                            candidate.remove_vertex(*vertex_id, graph)?;
                        }
                        DeltaOp::AddEdge(edge) => {
                            candidate.add_edge(edge.clone(), graph)?;
                        }
                        DeltaOp::RemoveEdge(edge_id) => {
                            candidate.remove_edge(*edge_id, graph)?;
                        }
                    }
                }
                remaining_version = remaining_version.checked_sub(1).ok_or_else(|| {
                    GraphStoreError::CorruptGraph("graph version underflow".to_string())
                })?;
                inverse_count = inverse_count.checked_add(1).ok_or_else(|| {
                    GraphStoreError::CorruptGraph("version history index overflow".to_string())
                })?;
            }
            Ok(())
        })?;
        self.version = remaining_version;
        let new_len = self
            .inverse_deltas
            .len()
            .checked_sub(inverse_count)
            .ok_or_else(|| {
                GraphStoreError::CorruptGraph("version history truncation underflow".to_string())
            })?;
        self.inverse_deltas.truncate(new_len);
        self.deltas.truncate(new_len);
        Ok(())
    }

    /// Register a callback fired with the set of affected edge labels
    /// every time `apply` lands a delta that touches at least one edge.
    pub fn on_invalidate<F>(&mut self, callback: F)
    where
        F: Fn(&BTreeSet<String>) + Send + Sync + 'static,
    {
        self.on_invalidate.push(Box::new(callback));
    }
}
