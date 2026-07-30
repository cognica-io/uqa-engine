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

pub struct VersionedGraphStore<'a, G: GraphStore> {
    base: &'a mut G,
    graph: String,
    version: u64,
    deltas: Vec<GraphDelta>,
    inverse_deltas: Vec<GraphDelta>,
    on_invalidate: Vec<InvalidationCallback>,
}

impl<'a, G: GraphStore + Clone> VersionedGraphStore<'a, G> {
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
        if !self.base.has_graph(&self.graph) {
            return Err(GraphStoreError::UnknownGraph(self.graph.clone()));
        }
        let next_version = self.version.checked_add(1).ok_or_else(|| {
            GraphStoreError::IdExhausted("graph version counter overflow".to_string())
        })?;
        let mut candidate = self.base.clone();
        let mut inverse = GraphDelta::new();
        let mut affected_labels = delta.affected_edge_labels();
        for op in delta.ops() {
            match op {
                DeltaOp::AddVertex(vertex) => {
                    candidate.add_vertex(vertex.clone(), &self.graph)?;
                    inverse.remove_vertex(vertex.vertex_id);
                }
                DeltaOp::RemoveVertex(vertex_id) => {
                    let existing = candidate.get_vertex(*vertex_id).cloned();
                    let incident_edges: Vec<_> = candidate
                        .edges_in_graph(&self.graph)?
                        .into_iter()
                        .filter(|edge| edge.source_id == *vertex_id || edge.target_id == *vertex_id)
                        .collect();
                    affected_labels.extend(incident_edges.iter().map(|edge| edge.label.clone()));
                    candidate.remove_vertex(*vertex_id, &self.graph)?;
                    for edge in incident_edges {
                        inverse.add_edge(edge);
                    }
                    if let Some(v) = existing {
                        inverse.add_vertex(v);
                    }
                }
                DeltaOp::AddEdge(edge) => {
                    candidate.add_edge(edge.clone(), &self.graph)?;
                    inverse.remove_edge(edge.edge_id);
                }
                DeltaOp::RemoveEdge(edge_id) => {
                    let existing = candidate.get_edge(*edge_id).cloned();
                    if let Some(edge) = &existing {
                        affected_labels.insert(edge.label.clone());
                    }
                    candidate.remove_edge(*edge_id, &self.graph)?;
                    if let Some(e) = existing {
                        inverse.add_edge(e);
                    }
                }
            }
        }
        *self.base = candidate;
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
        let mut candidate = self.base.clone();
        let mut remaining_version = self.version;
        let mut inverse_count = 0usize;
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
            for op in inverse.ops().iter().rev() {
                match op {
                    DeltaOp::AddVertex(vertex) => {
                        candidate.add_vertex(vertex.clone(), &self.graph)?;
                    }
                    DeltaOp::RemoveVertex(vertex_id) => {
                        candidate.remove_vertex(*vertex_id, &self.graph)?;
                    }
                    DeltaOp::AddEdge(edge) => {
                        candidate.add_edge(edge.clone(), &self.graph)?;
                    }
                    DeltaOp::RemoveEdge(edge_id) => {
                        candidate.remove_edge(*edge_id, &self.graph)?;
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
        *self.base = candidate;
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
