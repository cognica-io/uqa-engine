//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Graph runtime selection: memory is primary storage only for an in-memory
//! engine; durable engines retain only session-bound storage handles.

use crate::{
    Direction, GraphLabelInfo, GraphLabelRegistry, GraphStore, GraphStoreResult, LabelKind,
    MemoryGraphStore, PersistentGraphStore,
};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use uqa_core::{Edge, Vertex};
use uqa_storage::{CatalogFacade, PersistentStorageBackend};

#[derive(Debug, Clone)]
pub enum GraphStoreHandle {
    Memory(MemoryGraphStore),
    Persistent(PersistentGraphStore),
}

impl Default for GraphStoreHandle {
    fn default() -> Self {
        Self::Memory(MemoryGraphStore::new())
    }
}

impl GraphStoreHandle {
    pub fn from_catalog(
        catalog: Arc<dyn CatalogFacade>,
        backend: Arc<dyn PersistentStorageBackend>,
    ) -> Self {
        Self::Persistent(PersistentGraphStore::from_catalog(catalog, backend))
    }

    /// Preserve caller error types while storage checkpoints (or the primary
    /// memory store) roll back the complete operation on errors and panics.
    pub fn transaction_mapped<T, E>(
        &mut self,
        operation: impl FnOnce(&mut Self) -> Result<T, E>,
        map_error: impl Fn(crate::GraphStoreError) -> E,
    ) -> Result<T, E>
    where
        E: std::fmt::Display,
    {
        match self {
            Self::Memory(store) => {
                let backup = store.clone();
                match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| operation(self))) {
                    Ok(Ok(value)) => Ok(value),
                    Ok(Err(error)) => {
                        *self = Self::Memory(backup);
                        Err(error)
                    }
                    Err(panic) => {
                        *self = Self::Memory(backup);
                        std::panic::resume_unwind(panic)
                    }
                }
            }
            Self::Persistent(store) => {
                let mut checkpoint = store.clone();
                checkpoint.transaction_mapped(|_| operation(self), map_error)
            }
        }
    }

    pub fn label_registry(&self, graph: &str) -> GraphStoreResult<GraphLabelRegistry> {
        match self {
            Self::Memory(store) => Ok(store.label_registry(graph)),
            Self::Persistent(store) => store.label_registry(graph),
        }
    }
    pub fn graph_labels(&self, graph: &str) -> GraphStoreResult<Vec<GraphLabelInfo>> {
        match self {
            Self::Memory(store) => store.graph_labels(graph),
            Self::Persistent(store) => store.graph_labels(graph),
        }
    }
    pub fn graph_label_kind(
        &self,
        graph: &str,
        label: &str,
    ) -> GraphStoreResult<Option<LabelKind>> {
        match self {
            Self::Memory(store) => store.graph_label_kind(graph, label),
            Self::Persistent(store) => store.graph_label_kind(graph, label),
        }
    }
    pub fn create_label(
        &mut self,
        graph: &str,
        label: &str,
        kind: LabelKind,
    ) -> GraphStoreResult<Option<u32>> {
        match self {
            Self::Memory(store) => store.create_label(graph, label, kind),
            Self::Persistent(store) => store.create_label(graph, label, kind),
        }
    }
    pub fn drop_label(
        &mut self,
        graph: &str,
        label: &str,
    ) -> GraphStoreResult<Option<(u32, LabelKind)>> {
        match self {
            Self::Memory(store) => store.drop_label(graph, label),
            Self::Persistent(store) => store.drop_label(graph, label),
        }
    }
    pub fn rename_graph(&mut self, from: &str, to: &str) -> GraphStoreResult<()> {
        match self {
            Self::Memory(store) => store.rename_graph(from, to),
            Self::Persistent(store) => store.rename_graph(from, to),
        }
    }
    pub fn import_label_registry(
        &mut self,
        graph: &str,
        registry: &GraphLabelRegistry,
    ) -> GraphStoreResult<()> {
        match self {
            Self::Memory(store) => {
                store.import_label_registry(graph, registry);
                Ok(())
            }
            Self::Persistent(store) => store.import_label_registry(graph, registry),
        }
    }
    pub fn rebuild_label_registry_from_ids(&mut self, graph: &str) -> GraphStoreResult<()> {
        match self {
            Self::Memory(store) => {
                store.rebuild_label_registry_from_ids(graph);
                Ok(())
            }
            Self::Persistent(store) => store.rebuild_label_registry_from_ids(graph),
        }
    }
}

impl GraphStore for GraphStoreHandle {
    fn vertex_id_page(
        &self,
        graph: &str,
        after: Option<u64>,
        limit: usize,
    ) -> GraphStoreResult<Vec<u64>> {
        match self {
            Self::Memory(store) => store.vertex_id_page(graph, after, limit),
            Self::Persistent(store) => store.vertex_id_page(graph, after, limit),
        }
    }
    fn edge_id_page(
        &self,
        graph: &str,
        after: Option<u64>,
        limit: usize,
    ) -> GraphStoreResult<Vec<u64>> {
        match self {
            Self::Memory(store) => store.edge_id_page(graph, after, limit),
            Self::Persistent(store) => store.edge_id_page(graph, after, limit),
        }
    }
    fn transaction<T>(
        &mut self,
        operation: impl FnOnce(&mut Self) -> GraphStoreResult<T>,
    ) -> GraphStoreResult<T> {
        match self {
            Self::Memory(store) => {
                let backup = store.clone();
                match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| operation(self))) {
                    Ok(Ok(value)) => Ok(value),
                    Ok(Err(error)) => {
                        *self = Self::Memory(backup);
                        Err(error)
                    }
                    Err(panic) => {
                        *self = Self::Memory(backup);
                        std::panic::resume_unwind(panic)
                    }
                }
            }
            Self::Persistent(store) => {
                // This clone is a storage handle, never a graph snapshot.
                let mut checkpoint = store.clone();
                checkpoint.transaction(|_| operation(self))
            }
        }
    }
    fn create_graph(&mut self, name: &str) -> GraphStoreResult<()> {
        match self {
            Self::Memory(store) => GraphStore::create_graph(store, name),
            Self::Persistent(store) => GraphStore::create_graph(store, name),
        }
    }
    fn drop_graph(&mut self, name: &str) -> GraphStoreResult<()> {
        match self {
            Self::Memory(store) => GraphStore::drop_graph(store, name),
            Self::Persistent(store) => GraphStore::drop_graph(store, name),
        }
    }
    fn graph_names(&self) -> GraphStoreResult<Vec<String>> {
        match self {
            Self::Memory(store) => GraphStore::graph_names(store),
            Self::Persistent(store) => GraphStore::graph_names(store),
        }
    }
    fn has_graph(&self, name: &str) -> GraphStoreResult<bool> {
        match self {
            Self::Memory(store) => GraphStore::has_graph(store, name),
            Self::Persistent(store) => GraphStore::has_graph(store, name),
        }
    }
    fn union_graphs(&mut self, g1: &str, g2: &str, target: &str) -> GraphStoreResult<()> {
        match self {
            Self::Memory(store) => GraphStore::union_graphs(store, g1, g2, target),
            Self::Persistent(store) => GraphStore::union_graphs(store, g1, g2, target),
        }
    }
    fn intersect_graphs(&mut self, g1: &str, g2: &str, target: &str) -> GraphStoreResult<()> {
        match self {
            Self::Memory(store) => GraphStore::intersect_graphs(store, g1, g2, target),
            Self::Persistent(store) => GraphStore::intersect_graphs(store, g1, g2, target),
        }
    }
    fn difference_graphs(&mut self, g1: &str, g2: &str, target: &str) -> GraphStoreResult<()> {
        match self {
            Self::Memory(store) => GraphStore::difference_graphs(store, g1, g2, target),
            Self::Persistent(store) => GraphStore::difference_graphs(store, g1, g2, target),
        }
    }
    fn copy_graph(&mut self, source: &str, target: &str) -> GraphStoreResult<()> {
        match self {
            Self::Memory(store) => GraphStore::copy_graph(store, source, target),
            Self::Persistent(store) => GraphStore::copy_graph(store, source, target),
        }
    }
    fn add_vertex(&mut self, vertex: Vertex, graph: &str) -> GraphStoreResult<()> {
        match self {
            Self::Memory(store) => GraphStore::add_vertex(store, vertex, graph),
            Self::Persistent(store) => GraphStore::add_vertex(store, vertex, graph),
        }
    }
    fn add_edge(&mut self, edge: Edge, graph: &str) -> GraphStoreResult<()> {
        match self {
            Self::Memory(store) => GraphStore::add_edge(store, edge, graph),
            Self::Persistent(store) => GraphStore::add_edge(store, edge, graph),
        }
    }
    fn remove_vertex(&mut self, vertex_id: u64, graph: &str) -> GraphStoreResult<()> {
        match self {
            Self::Memory(store) => GraphStore::remove_vertex(store, vertex_id, graph),
            Self::Persistent(store) => GraphStore::remove_vertex(store, vertex_id, graph),
        }
    }
    fn remove_edge(&mut self, edge_id: u64, graph: &str) -> GraphStoreResult<()> {
        match self {
            Self::Memory(store) => GraphStore::remove_edge(store, edge_id, graph),
            Self::Persistent(store) => GraphStore::remove_edge(store, edge_id, graph),
        }
    }
    fn neighbors(
        &self,
        vertex_id: u64,
        label: Option<&str>,
        direction: Direction,
        graph: &str,
    ) -> GraphStoreResult<Vec<u64>> {
        match self {
            Self::Memory(store) => GraphStore::neighbors(store, vertex_id, label, direction, graph),
            Self::Persistent(store) => {
                GraphStore::neighbors(store, vertex_id, label, direction, graph)
            }
        }
    }
    fn vertices_by_label(&self, label: &str, graph: &str) -> GraphStoreResult<Vec<Vertex>> {
        match self {
            Self::Memory(store) => GraphStore::vertices_by_label(store, label, graph),
            Self::Persistent(store) => GraphStore::vertices_by_label(store, label, graph),
        }
    }
    fn vertex_ids_by_label(&self, label: &str, graph: &str) -> GraphStoreResult<Vec<u64>> {
        match self {
            Self::Memory(store) => GraphStore::vertex_ids_by_label(store, label, graph),
            Self::Persistent(store) => GraphStore::vertex_ids_by_label(store, label, graph),
        }
    }
    fn vertices_in_graph(&self, graph: &str) -> GraphStoreResult<Vec<Vertex>> {
        match self {
            Self::Memory(store) => GraphStore::vertices_in_graph(store, graph),
            Self::Persistent(store) => GraphStore::vertices_in_graph(store, graph),
        }
    }
    fn edges_in_graph(&self, graph: &str) -> GraphStoreResult<Vec<Edge>> {
        match self {
            Self::Memory(store) => GraphStore::edges_in_graph(store, graph),
            Self::Persistent(store) => GraphStore::edges_in_graph(store, graph),
        }
    }
    fn vertex_graphs(&self, vertex_id: u64) -> GraphStoreResult<BTreeSet<String>> {
        match self {
            Self::Memory(store) => GraphStore::vertex_graphs(store, vertex_id),
            Self::Persistent(store) => GraphStore::vertex_graphs(store, vertex_id),
        }
    }
    fn edge_graphs(&self, edge_id: u64) -> GraphStoreResult<BTreeSet<String>> {
        match self {
            Self::Memory(store) => store.edge_graphs(edge_id),
            Self::Persistent(store) => store.edge_graphs(edge_id),
        }
    }
    fn edges_by_label(&self, label: &str, graph: &str) -> GraphStoreResult<Vec<Edge>> {
        match self {
            Self::Memory(store) => store.edges_by_label(label, graph),
            Self::Persistent(store) => store.edges_by_label(label, graph),
        }
    }
    fn out_edge_ids(&self, vertex_id: u64, graph: &str) -> GraphStoreResult<BTreeSet<u64>> {
        match self {
            Self::Memory(store) => GraphStore::out_edge_ids(store, vertex_id, graph),
            Self::Persistent(store) => GraphStore::out_edge_ids(store, vertex_id, graph),
        }
    }
    fn in_edge_ids(&self, vertex_id: u64, graph: &str) -> GraphStoreResult<BTreeSet<u64>> {
        match self {
            Self::Memory(store) => GraphStore::in_edge_ids(store, vertex_id, graph),
            Self::Persistent(store) => GraphStore::in_edge_ids(store, vertex_id, graph),
        }
    }
    fn edge_ids_by_label(&self, label: &str, graph: &str) -> GraphStoreResult<BTreeSet<u64>> {
        match self {
            Self::Memory(store) => GraphStore::edge_ids_by_label(store, label, graph),
            Self::Persistent(store) => GraphStore::edge_ids_by_label(store, label, graph),
        }
    }
    fn vertex_ids_in_graph(&self, graph: &str) -> GraphStoreResult<BTreeSet<u64>> {
        match self {
            Self::Memory(store) => GraphStore::vertex_ids_in_graph(store, graph),
            Self::Persistent(store) => GraphStore::vertex_ids_in_graph(store, graph),
        }
    }
    fn require_vertex_in_graph(&self, vertex_id: u64, graph: &str) -> GraphStoreResult<()> {
        match self {
            Self::Memory(store) => GraphStore::require_vertex_in_graph(store, vertex_id, graph),
            Self::Persistent(store) => GraphStore::require_vertex_in_graph(store, vertex_id, graph),
        }
    }
    fn degree_distribution(&self, graph: &str) -> GraphStoreResult<BTreeMap<u64, u64>> {
        match self {
            Self::Memory(store) => GraphStore::degree_distribution(store, graph),
            Self::Persistent(store) => GraphStore::degree_distribution(store, graph),
        }
    }
    fn label_degree(&self, label: &str, graph: &str) -> GraphStoreResult<f64> {
        match self {
            Self::Memory(store) => GraphStore::label_degree(store, label, graph),
            Self::Persistent(store) => GraphStore::label_degree(store, label, graph),
        }
    }
    fn vertex_label_counts(&self, graph: &str) -> GraphStoreResult<BTreeMap<String, u64>> {
        match self {
            Self::Memory(store) => GraphStore::vertex_label_counts(store, graph),
            Self::Persistent(store) => GraphStore::vertex_label_counts(store, graph),
        }
    }
    fn get_vertex(&self, vertex_id: u64) -> GraphStoreResult<Option<Vertex>> {
        match self {
            Self::Memory(store) => GraphStore::get_vertex(store, vertex_id),
            Self::Persistent(store) => GraphStore::get_vertex(store, vertex_id),
        }
    }
    fn get_edge(&self, edge_id: u64) -> GraphStoreResult<Option<Edge>> {
        match self {
            Self::Memory(store) => GraphStore::get_edge(store, edge_id),
            Self::Persistent(store) => GraphStore::get_edge(store, edge_id),
        }
    }
    fn next_vertex_id(&mut self) -> GraphStoreResult<u64> {
        match self {
            Self::Memory(store) => GraphStore::next_vertex_id(store),
            Self::Persistent(store) => GraphStore::next_vertex_id(store),
        }
    }
    fn next_edge_id(&mut self) -> GraphStoreResult<u64> {
        match self {
            Self::Memory(store) => GraphStore::next_edge_id(store),
            Self::Persistent(store) => GraphStore::next_edge_id(store),
        }
    }
    fn allocate_vertex_id(&mut self, label: &str, graph: &str) -> GraphStoreResult<u64> {
        match self {
            Self::Memory(store) => GraphStore::allocate_vertex_id(store, label, graph),
            Self::Persistent(store) => GraphStore::allocate_vertex_id(store, label, graph),
        }
    }
    fn allocate_edge_id(&mut self, label: &str, graph: &str) -> GraphStoreResult<u64> {
        match self {
            Self::Memory(store) => GraphStore::allocate_edge_id(store, label, graph),
            Self::Persistent(store) => GraphStore::allocate_edge_id(store, label, graph),
        }
    }
    fn clear(&mut self) -> GraphStoreResult<()> {
        match self {
            Self::Memory(store) => GraphStore::clear(store),
            Self::Persistent(store) => GraphStore::clear(store),
        }
    }
    fn vertices(&self) -> GraphStoreResult<BTreeMap<u64, Vertex>> {
        match self {
            Self::Memory(store) => GraphStore::vertices(store),
            Self::Persistent(store) => GraphStore::vertices(store),
        }
    }
    fn edges(&self) -> GraphStoreResult<BTreeMap<u64, Edge>> {
        match self {
            Self::Memory(store) => GraphStore::edges(store),
            Self::Persistent(store) => GraphStore::edges(store),
        }
    }
}
