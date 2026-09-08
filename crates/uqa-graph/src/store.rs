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

use crate::posting_list::GraphPostingListError;
use crate::types::Direction;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GraphStoreError {
    #[error("graph {0:?} does not exist")]
    UnknownGraph(String),
    #[error("graph id space exhausted: {0}")]
    IdExhausted(String),
    #[error("invalid graph mutation: {0}")]
    InvalidMutation(String),
    #[error("invalid graph query: {0}")]
    InvalidQuery(String),
    #[error("corrupt graph state: {0}")]
    CorruptGraph(String),
    #[error("graph storage error: {0}")]
    Storage(String),
    #[error("graph serialization failure: {0}")]
    SerializationFailure(String),
    #[error(transparent)]
    InvalidPostingList(#[from] GraphPostingListError),
}

pub type GraphStoreResult<T> = Result<T, GraphStoreError>;

/// Storage interface for named property graphs.
///
/// Each store hosts zero or more named graphs that share a single
/// vertex / edge id space (a vertex can belong to multiple graphs).
/// Mutations are scoped to a target graph by name.
pub trait GraphStore {
    /// Apply a group of mutations atomically. Persistent stores use a storage
    /// transaction or savepoint, never a cloned graph as a rollback image.
    /// Both errors and unwinding must restore the pre-operation state.
    fn transaction<T>(
        &mut self,
        operation: impl FnOnce(&mut Self) -> GraphStoreResult<T>,
    ) -> GraphStoreResult<T>
    where
        Self: Sized;

    // --- Lifecycle ---

    /// Create a new named graph. No-op if it already exists.
    fn create_graph(&mut self, name: &str) -> GraphStoreResult<()>;

    /// Drop a named graph and all of its membership entries. Vertex /
    /// edge records that aren't referenced by any other graph become
    /// unreachable and are released.
    fn drop_graph(&mut self, name: &str) -> GraphStoreResult<()>;

    /// Return all graph names sorted ascending.
    fn graph_names(&self) -> GraphStoreResult<Vec<String>>;

    fn has_graph(&self, name: &str) -> GraphStoreResult<bool>;

    // --- Algebra ---

    /// `target := g1 union g2` over vertex and edge sets.
    fn union_graphs(&mut self, g1: &str, g2: &str, target: &str) -> GraphStoreResult<()>;

    /// `target := g1 intersect g2`.
    fn intersect_graphs(&mut self, g1: &str, g2: &str, target: &str) -> GraphStoreResult<()>;

    /// `target := g1 \ g2`.
    fn difference_graphs(&mut self, g1: &str, g2: &str, target: &str) -> GraphStoreResult<()>;

    fn copy_graph(&mut self, source: &str, target: &str) -> GraphStoreResult<()>;

    // --- Mutations ---

    fn add_vertex(&mut self, vertex: Vertex, graph: &str) -> GraphStoreResult<()>;

    fn add_edge(&mut self, edge: Edge, graph: &str) -> GraphStoreResult<()>;

    fn remove_vertex(&mut self, vertex_id: VertexId, graph: &str) -> GraphStoreResult<()>;

    fn remove_edge(&mut self, edge_id: EdgeId, graph: &str) -> GraphStoreResult<()>;

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
    ) -> GraphStoreResult<Vec<VertexId>>;

    fn vertices_by_label(&self, label: &str, graph: &str) -> GraphStoreResult<Vec<Vertex>>;

    /// Return only the vertex ids for a label. Stores with a label index should override this to avoid materializing full vertices.
    fn vertex_ids_by_label(&self, label: &str, graph: &str) -> GraphStoreResult<Vec<VertexId>> {
        Ok(self
            .vertices_by_label(label, graph)?
            .into_iter()
            .map(|vertex| vertex.vertex_id)
            .collect())
    }

    fn vertices_in_graph(&self, graph: &str) -> GraphStoreResult<Vec<Vertex>>;

    fn edges_in_graph(&self, graph: &str) -> GraphStoreResult<Vec<Edge>>;

    /// Read only edges carrying a label. Durable stores push this predicate
    /// into their physical label index before fetching any payloads.
    fn edges_by_label(&self, label: &str, graph: &str) -> GraphStoreResult<Vec<Edge>> {
        self.edge_ids_by_label(label, graph)?
            .into_iter()
            .map(|id| {
                self.get_edge(id)?
                    .ok_or_else(|| GraphStoreError::CorruptGraph(format!("missing edge {id}")))
            })
            .collect()
    }

    fn vertex_graphs(&self, vertex_id: VertexId) -> GraphStoreResult<BTreeSet<String>>;
    fn edge_graphs(&self, edge_id: EdgeId) -> GraphStoreResult<BTreeSet<String>>;

    // --- Adjacency accessors ---

    fn out_edge_ids(&self, vertex_id: VertexId, graph: &str) -> GraphStoreResult<BTreeSet<EdgeId>>;

    fn in_edge_ids(&self, vertex_id: VertexId, graph: &str) -> GraphStoreResult<BTreeSet<EdgeId>>;

    fn edge_ids_by_label(&self, label: &str, graph: &str) -> GraphStoreResult<BTreeSet<EdgeId>>;

    fn vertex_ids_in_graph(&self, graph: &str) -> GraphStoreResult<BTreeSet<VertexId>>;

    /// Bounded ascending vertex identities, exclusively after the cursor.
    /// Durable stores override this with an indexed storage cursor.
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
            .vertex_ids_in_graph(graph)?
            .into_iter()
            .filter(|id| after.is_none_or(|after| *id > after))
            .take(limit)
            .collect())
    }

    /// Bounded ascending edge identities without materializing edge payloads.
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
        let mut ids = BTreeSet::new();
        for vertex in self.vertex_ids_in_graph(graph)? {
            ids.extend(self.out_edge_ids(vertex, graph)?);
        }
        Ok(ids
            .into_iter()
            .filter(|id| after.is_none_or(|after| *id > after))
            .take(limit)
            .collect())
    }

    /// Require an explicit query vertex to be a live member of `graph`.
    /// Implementations may override this with a cheaper membership lookup.
    /// Missing query input is distinct from a valid vertex with no edges and
    /// must not be reported as an empty neighborhood/path result.
    fn require_vertex_in_graph(&self, vertex_id: VertexId, graph: &str) -> GraphStoreResult<()> {
        if !self.vertex_ids_in_graph(graph)?.contains(&vertex_id) {
            return Err(GraphStoreError::InvalidQuery(format!(
                "vertex {vertex_id} is not a member of graph {graph:?}"
            )));
        }
        if self.get_vertex(vertex_id)?.is_none() {
            return Err(GraphStoreError::CorruptGraph(format!(
                "graph {graph:?} references missing vertex {vertex_id}"
            )));
        }
        Ok(())
    }

    // --- Statistics ---

    fn degree_distribution(&self, graph: &str) -> GraphStoreResult<BTreeMap<VertexId, u64>>;

    fn label_degree(&self, label: &str, graph: &str) -> GraphStoreResult<f64>;

    fn vertex_label_counts(&self, graph: &str) -> GraphStoreResult<BTreeMap<String, u64>>;

    // --- Global accessors ---

    /// Fetch one owned record. A storage implementation must not retain all
    /// entities merely to return a borrow, and I/O failures are not absence.
    fn get_vertex(&self, vertex_id: VertexId) -> GraphStoreResult<Option<Vertex>>;

    fn get_edge(&self, edge_id: EdgeId) -> GraphStoreResult<Option<Edge>>;

    /// Returns and advances the next available vertex id.
    fn next_vertex_id(&mut self) -> GraphStoreResult<VertexId>;

    /// Returns and advances the next available edge id.
    fn next_edge_id(&mut self) -> GraphStoreResult<EdgeId>;

    /// Allocate a vertex id for a new entity with `label` inside
    /// `graph`. Stores that implement the Apache AGE `graphid` scheme
    /// override this to return `(label_id << 48) | sequence`; the
    /// default falls back to the store-wide counter.
    fn allocate_vertex_id(&mut self, _label: &str, _graph: &str) -> GraphStoreResult<VertexId> {
        self.next_vertex_id()
    }

    /// Allocate an edge id for a new entity with `label` inside
    /// `graph`. See [`GraphStore::allocate_vertex_id`].
    fn allocate_edge_id(&mut self, _label: &str, _graph: &str) -> GraphStoreResult<EdgeId> {
        self.next_edge_id()
    }

    fn clear(&mut self) -> GraphStoreResult<()>;

    // --- Bulk accessors ---

    /// Snapshot every vertex in the store, keyed by id. Mirrors the
    /// `vertices` property on the current abstract `GraphStore`.
    fn vertices(&self) -> GraphStoreResult<BTreeMap<VertexId, Vertex>>;

    /// Snapshot every edge in the store, keyed by id. Mirrors the
    /// `edges` property on the current abstract `GraphStore`.
    fn edges(&self) -> GraphStoreResult<BTreeMap<EdgeId, Edge>>;
}

impl From<uqa_storage::StorageBackendError> for GraphStoreError {
    fn from(error: uqa_storage::StorageBackendError) -> Self {
        Self::Storage(error.to_string())
    }
}
