//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Graph deltas: a sequence of add/remove vertex/edge operations
//! applied atomically to a [`GraphStore`]. Deltas drive the
//! [`crate::VersionedGraphStore`] versioning model and feed targeted
//! path-index invalidation by exposing affected vertex ids and edge
//! labels.

use std::collections::BTreeSet;

use uqa_core::{Edge, EdgeId, Vertex, VertexId};

/// A single mutation operation in a [`GraphDelta`].
#[derive(Debug, Clone)]
pub enum DeltaOp {
    AddVertex(Vertex),
    RemoveVertex(VertexId),
    AddEdge(Edge),
    RemoveEdge(EdgeId),
}

/// Records add / remove vertex / edge operations (Section 9.3, Paper 2).
#[derive(Debug, Clone, Default)]
pub struct GraphDelta {
    ops: Vec<DeltaOp>,
}

impl GraphDelta {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_vertex(&mut self, vertex: Vertex) {
        self.ops.push(DeltaOp::AddVertex(vertex));
    }

    pub fn remove_vertex(&mut self, vertex_id: VertexId) {
        self.ops.push(DeltaOp::RemoveVertex(vertex_id));
    }

    pub fn add_edge(&mut self, edge: Edge) {
        self.ops.push(DeltaOp::AddEdge(edge));
    }

    pub fn remove_edge(&mut self, edge_id: EdgeId) {
        self.ops.push(DeltaOp::RemoveEdge(edge_id));
    }

    pub fn ops(&self) -> &[DeltaOp] {
        &self.ops
    }

    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    pub fn len(&self) -> usize {
        self.ops.len()
    }

    /// Set of vertex ids touched by any op (vertices added / removed,
    /// plus the source / target of added edges). Edge removal does not
    /// resurrect a vertex id since the operation only stores the edge
    /// id.
    pub fn affected_vertex_ids(&self) -> BTreeSet<VertexId> {
        let mut ids = BTreeSet::new();
        for op in &self.ops {
            match op {
                DeltaOp::AddVertex(v) => {
                    ids.insert(v.vertex_id);
                }
                DeltaOp::RemoveVertex(v) => {
                    ids.insert(*v);
                }
                DeltaOp::AddEdge(e) => {
                    ids.insert(e.source_id);
                    ids.insert(e.target_id);
                }
                DeltaOp::RemoveEdge(_) => {}
            }
        }
        ids
    }

    /// Set of edge labels touched by `AddEdge` ops. Used by the
    /// versioned store to invalidate path indexes that depend on a
    /// label.
    pub fn affected_edge_labels(&self) -> BTreeSet<String> {
        let mut labels = BTreeSet::new();
        for op in &self.ops {
            if let DeltaOp::AddEdge(edge) = op {
                labels.insert(edge.label.clone());
            }
        }
        labels
    }
}
