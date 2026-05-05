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
use crate::store::GraphStore;

type InvalidationCallback = Box<dyn Fn(&BTreeSet<String>) + Send + Sync>;

pub struct VersionedGraphStore<'a, G: GraphStore> {
    base: &'a mut G,
    graph: String,
    version: u64,
    deltas: Vec<GraphDelta>,
    inverse_deltas: Vec<GraphDelta>,
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
    pub fn apply(&mut self, delta: GraphDelta) -> u64 {
        let mut inverse = GraphDelta::new();
        for op in delta.ops() {
            match op {
                DeltaOp::AddVertex(vertex) => {
                    self.base.add_vertex(vertex.clone(), &self.graph);
                    inverse.remove_vertex(vertex.vertex_id);
                }
                DeltaOp::RemoveVertex(vertex_id) => {
                    let existing = self.base.get_vertex(*vertex_id).cloned();
                    self.base.remove_vertex(*vertex_id, &self.graph);
                    if let Some(v) = existing {
                        inverse.add_vertex(v);
                    }
                }
                DeltaOp::AddEdge(edge) => {
                    self.base.add_edge(edge.clone(), &self.graph);
                    inverse.remove_edge(edge.edge_id);
                }
                DeltaOp::RemoveEdge(edge_id) => {
                    let existing = self.base.get_edge(*edge_id).cloned();
                    self.base.remove_edge(*edge_id, &self.graph);
                    if let Some(e) = existing {
                        inverse.add_edge(e);
                    }
                }
            }
        }
        self.version += 1;
        let labels = delta.affected_edge_labels();
        self.deltas.push(delta);
        self.inverse_deltas.push(inverse);
        if !labels.is_empty() {
            for callback in &self.on_invalidate {
                callback(&labels);
            }
        }
        self.version
    }

    /// Rewind to the given version by replaying inverse deltas. Errors
    /// when the target version is in the future or below zero.
    pub fn rollback(&mut self, to_version: u64) -> Result<(), String> {
        if to_version > self.version {
            return Err(format!(
                "cannot rollback to version {to_version} (current: {})",
                self.version
            ));
        }
        while self.version > to_version {
            let Some(inverse) = self.inverse_deltas.pop() else {
                break;
            };
            self.deltas.pop();
            for op in inverse.ops() {
                match op {
                    DeltaOp::AddVertex(vertex) => {
                        self.base.add_vertex(vertex.clone(), &self.graph);
                    }
                    DeltaOp::RemoveVertex(vertex_id) => {
                        self.base.remove_vertex(*vertex_id, &self.graph);
                    }
                    DeltaOp::AddEdge(edge) => {
                        self.base.add_edge(edge.clone(), &self.graph);
                    }
                    DeltaOp::RemoveEdge(edge_id) => {
                        self.base.remove_edge(*edge_id, &self.graph);
                    }
                }
            }
            self.version -= 1;
        }
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
