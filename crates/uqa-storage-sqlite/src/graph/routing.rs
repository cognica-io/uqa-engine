//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Existing handles follow their connection when it becomes bound to native logical records.

use super::{access::SQLiteGraphStorage, native::NativeGraphStorage};
use crate::Result;
use uqa_core::{Edge, Vertex};
use uqa_graph::{GraphLabelRegistry, GraphStorage, GraphStoreResult, GraphWriteTransaction};
use uqa_storage::{GraphEntityFilter, GraphEntityKind};

pub(super) struct RoutedGraphStorage {
    pub legacy: SQLiteGraphStorage,
    pub native: NativeGraphStorage,
}

impl RoutedGraphStorage {
    fn current(&self) -> &dyn GraphStorage {
        if self.legacy.conn.is_native_record_session() {
            &self.native
        } else {
            &self.legacy
        }
    }
    pub(super) fn ensure_tables(&self) -> Result<()> {
        if self.legacy.conn.is_native_record_session() {
            self.native.ensure_tables()
        } else {
            self.legacy.ensure_tables()
        }
    }
    pub(super) fn metadata(&self, key: &str) -> Result<Option<String>> {
        if self.legacy.conn.is_native_record_session() {
            self.native.metadata(key)
        } else {
            self.legacy.metadata(key)
        }
    }
    pub(super) fn save_metadata(&self, key: &str, value: &str) -> Result<()> {
        if self.legacy.conn.is_native_record_session() {
            self.native.save_metadata(key, value)
        } else {
            self.legacy.save_metadata(key, value)
        }
    }
}

impl GraphStorage for RoutedGraphStorage {
    fn begin_write(&self) -> GraphStoreResult<Box<dyn GraphWriteTransaction>> {
        self.current().begin_write()
    }
    fn graph_names(&self) -> GraphStoreResult<Vec<String>> {
        self.current().graph_names()
    }
    fn has_graph(&self, graph: &str) -> GraphStoreResult<bool> {
        self.current().has_graph(graph)
    }
    fn create_graph(&self, graph: &str) -> GraphStoreResult<()> {
        self.current().create_graph(graph)
    }
    fn delete_graph(&self, graph: &str) -> GraphStoreResult<()> {
        self.current().delete_graph(graph)
    }
    fn registry(&self, graph: &str) -> GraphStoreResult<GraphLabelRegistry> {
        self.current().registry(graph)
    }
    fn save_registry(&self, graph: &str, registry: &GraphLabelRegistry) -> GraphStoreResult<()> {
        self.current().save_registry(graph, registry)
    }
    fn counter(&self, kind: GraphEntityKind) -> GraphStoreResult<Option<u64>> {
        self.current().counter(kind)
    }
    fn save_counter(&self, kind: GraphEntityKind, next: u64) -> GraphStoreResult<()> {
        self.current().save_counter(kind, next)
    }
    fn vertex(&self, id: u64) -> GraphStoreResult<Option<Vertex>> {
        self.current().vertex(id)
    }
    fn edge(&self, id: u64) -> GraphStoreResult<Option<Edge>> {
        self.current().edge(id)
    }
    fn save_vertex(&self, vertex: &Vertex) -> GraphStoreResult<()> {
        self.current().save_vertex(vertex)
    }
    fn save_edge(&self, edge: &Edge) -> GraphStoreResult<()> {
        self.current().save_edge(edge)
    }
    fn delete_vertex(&self, id: u64) -> GraphStoreResult<()> {
        self.current().delete_vertex(id)
    }
    fn delete_edge(&self, id: u64) -> GraphStoreResult<()> {
        self.current().delete_edge(id)
    }
    fn ids(
        &self,
        filter: GraphEntityFilter<'_>,
        after: Option<u64>,
        limit: usize,
    ) -> GraphStoreResult<Vec<u64>> {
        self.current().ids(filter, after, limit)
    }
    fn count(&self, filter: GraphEntityFilter<'_>) -> GraphStoreResult<u64> {
        self.current().count(filter)
    }
    fn max_id(&self, kind: GraphEntityKind) -> GraphStoreResult<Option<u64>> {
        self.current().max_id(kind)
    }
    fn memberships(&self, kind: GraphEntityKind, id: u64) -> GraphStoreResult<Vec<String>> {
        self.current().memberships(kind, id)
    }
    fn has_membership(
        &self,
        kind: GraphEntityKind,
        id: u64,
        graph: &str,
    ) -> GraphStoreResult<bool> {
        self.current().has_membership(kind, id, graph)
    }
    fn attach(&self, kind: GraphEntityKind, id: u64, graph: &str) -> GraphStoreResult<()> {
        self.current().attach(kind, id, graph)
    }
    fn detach(&self, kind: GraphEntityKind, id: u64, graph: &str) -> GraphStoreResult<()> {
        self.current().detach(kind, id, graph)
    }
}
