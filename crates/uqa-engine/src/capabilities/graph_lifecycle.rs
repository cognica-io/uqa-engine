//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind graph command execution to live catalogs and public graph transaction entry points.

use crate::Engine;
use uqa_execution::query::graph_lifecycle::GraphLifecycle;
use uqa_graph::{GraphLabelInfo, LabelKind};
use uqa_storage::StorageBackendResult;

impl GraphLifecycle for Engine {
    fn has_graph(&self, name: &str) -> StorageBackendResult<bool> {
        self.has_graph(name)
    }
    fn has_namespace(&self, name: &str) -> StorageBackendResult<bool> {
        self.has_namespace(name)
    }
    fn create_graph(&self, name: String) -> StorageBackendResult<bool> {
        self.create_graph(name)
    }
    fn drop_graph(&self, name: &str) -> StorageBackendResult<bool> {
        self.drop_graph(name)
    }
    fn list_graph_labels(&self, graph: &str) -> StorageBackendResult<Option<Vec<GraphLabelInfo>>> {
        self.list_graph_labels(graph)
    }
    fn create_graph_label(
        &self,
        graph: &str,
        label: &str,
        kind: LabelKind,
    ) -> StorageBackendResult<bool> {
        self.create_graph_label(graph, label, kind)
    }
    fn drop_graph_label(&self, graph: &str, label: &str) -> StorageBackendResult<bool> {
        self.drop_graph_label(graph, label)
    }
    fn graph_label_relation_dependents(
        &self,
        graph: &str,
        label: &str,
    ) -> StorageBackendResult<Vec<String>> {
        self.graph_label_relation_dependents(graph, label)
    }
    fn rename_graph(&self, from: &str, to: &str) -> StorageBackendResult<bool> {
        self.rename_graph(from, to)
    }
}
