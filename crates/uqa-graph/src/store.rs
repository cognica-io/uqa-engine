//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `GraphStore` trait — the abstract storage interface for named
//! property graphs. Both an in-memory store and a SQLite-backed store
//! sit behind it.

use std::collections::{BTreeMap, BTreeSet};

use uqa_core::{Edge, EdgeId, Vertex, VertexId};

use crate::types::Direction;

/// Storage interface for named property graphs.
///
/// Each store hosts zero or more named graphs that share a single
/// vertex / edge id space (a vertex can belong to multiple graphs).
/// Mutations are scoped to a target graph by name.
pub trait GraphStore {
    // --- Lifecycle ---

    /// Create a new named graph. No-op if it already exists.
    fn create_graph(&mut self, name: &str);

    /// Drop a named graph and all of its membership entries. Vertex /
    /// edge records that aren't referenced by any other graph become
    /// unreachable and are released.
    fn drop_graph(&mut self, name: &str);

    /// Return all graph names sorted ascending.
    fn graph_names(&self) -> Vec<String>;

    fn has_graph(&self, name: &str) -> bool;

    // --- Algebra ---

    /// `target := g1 union g2` over vertex and edge sets.
    fn union_graphs(&mut self, g1: &str, g2: &str, target: &str);

    /// `target := g1 intersect g2`.
    fn intersect_graphs(&mut self, g1: &str, g2: &str, target: &str);

    /// `target := g1 \ g2`.
    fn difference_graphs(&mut self, g1: &str, g2: &str, target: &str);

    fn copy_graph(&mut self, source: &str, target: &str);

    // --- Mutations ---

    fn add_vertex(&mut self, vertex: Vertex, graph: &str);

    fn add_edge(&mut self, edge: Edge, graph: &str);

    fn remove_vertex(&mut self, vertex_id: VertexId, graph: &str);

    fn remove_edge(&mut self, edge_id: EdgeId, graph: &str);

    // --- Queries ---

    /// Neighbor vertex ids reached from `vertex_id` along edges with the
    /// given label (or any label when `label` is `None`) in the given
    /// direction.
    fn neighbors(
        &self,
        vertex_id: VertexId,
        label: Option<&str>,
        direction: Direction,
        graph: &str,
    ) -> Vec<VertexId>;

    fn vertices_by_label(&self, label: &str, graph: &str) -> Vec<Vertex>;

    /// Return only the vertex ids for a label. Stores with a label index should override this to avoid materializing full vertices.
    fn vertex_ids_by_label(&self, label: &str, graph: &str) -> Vec<VertexId> {
        self.vertices_by_label(label, graph)
            .into_iter()
            .map(|vertex| vertex.vertex_id)
            .collect()
    }

    fn vertices_in_graph(&self, graph: &str) -> Vec<Vertex>;

    fn edges_in_graph(&self, graph: &str) -> Vec<Edge>;

    fn vertex_graphs(&self, vertex_id: VertexId) -> BTreeSet<String>;

    // --- Adjacency accessors ---

    fn out_edge_ids(&self, vertex_id: VertexId, graph: &str) -> BTreeSet<EdgeId>;

    fn in_edge_ids(&self, vertex_id: VertexId, graph: &str) -> BTreeSet<EdgeId>;

    fn edge_ids_by_label(&self, label: &str, graph: &str) -> BTreeSet<EdgeId>;

    fn vertex_ids_in_graph(&self, graph: &str) -> BTreeSet<VertexId>;

    // --- Statistics ---

    fn degree_distribution(&self, graph: &str) -> BTreeMap<VertexId, u64>;

    fn label_degree(&self, label: &str, graph: &str) -> f64;

    fn vertex_label_counts(&self, graph: &str) -> BTreeMap<String, u64>;

    // --- Global accessors ---

    fn get_vertex(&self, vertex_id: VertexId) -> Option<&Vertex>;

    fn get_edge(&self, edge_id: EdgeId) -> Option<&Edge>;

    /// Returns and advances the next available vertex id.
    fn next_vertex_id(&mut self) -> VertexId;

    /// Returns and advances the next available edge id.
    fn next_edge_id(&mut self) -> EdgeId;

    /// Allocate a vertex id for a new entity with `label` inside
    /// `graph`. Stores that implement the Apache AGE `graphid` scheme
    /// override this to return `(label_id << 48) | sequence`; the
    /// default falls back to the store-wide counter.
    fn allocate_vertex_id(&mut self, _label: &str, _graph: &str) -> VertexId {
        self.next_vertex_id()
    }

    /// Allocate an edge id for a new entity with `label` inside
    /// `graph`. See [`GraphStore::allocate_vertex_id`].
    fn allocate_edge_id(&mut self, _label: &str, _graph: &str) -> EdgeId {
        self.next_edge_id()
    }

    fn clear(&mut self);

    // --- Bulk accessors ---

    /// Snapshot every vertex in the store, keyed by id. Mirrors the
    /// `vertices` property on the canonical UQA implementation's abstract `GraphStore`.
    fn vertices(&self) -> BTreeMap<VertexId, Vertex>;

    /// Snapshot every edge in the store, keyed by id. Mirrors the
    /// `edges` property on the canonical UQA implementation's abstract `GraphStore`.
    fn edges(&self) -> BTreeMap<EdgeId, Edge>;
}
