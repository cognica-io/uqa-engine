//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Property-graph vertex and edge value types.

use super::{BTreeMap, Value};

/// Stable identifier for a graph vertex. `u64` keeps adjacency entries
/// compact and fits up to 1.8e19 vertices per graph store.
pub type VertexId = u64;

/// Stable identifier for a graph edge.
pub type EdgeId = u64;

/// Property graph vertex: `(id, label, properties)`. Properties are typed
/// by [`Value`] so vertex props share the same encoding as document fields.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Vertex {
    pub vertex_id: VertexId,
    pub label: String,
    pub properties: BTreeMap<String, Value>,
}

impl Vertex {
    pub fn new(vertex_id: VertexId, label: impl Into<String>) -> Self {
        Self {
            vertex_id,
            label: label.into(),
            properties: BTreeMap::new(),
        }
    }
}

/// Directed property graph edge: `(id, source, target, label, properties)`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Edge {
    pub edge_id: EdgeId,
    pub source_id: VertexId,
    pub target_id: VertexId,
    pub label: String,
    pub properties: BTreeMap<String, Value>,
}

impl Edge {
    pub fn new(
        edge_id: EdgeId,
        source_id: VertexId,
        target_id: VertexId,
        label: impl Into<String>,
    ) -> Self {
        Self {
            edge_id,
            source_id,
            target_id,
            label: label.into(),
            properties: BTreeMap::new(),
        }
    }
}
