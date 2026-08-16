//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    BTreeMap, EdgeRow, Engine, GraphSnapshot, GraphVertexRow, StorageBackendResult, Value,
};

fn graph_store_error(error: impl std::fmt::Display) -> super::StorageBackendError {
    super::StorageBackendError::Other(error.to_string())
}

impl Engine {
    pub fn create_graph(&self, name: impl Into<String>) -> StorageBackendResult<bool> {
        let name = name.into();
        self.with_implicit_storage_transaction(|engine| engine.create_graph_inner(&name))
    }

    fn create_graph_inner(&self, name: &str) -> StorageBackendResult<bool> {
        use uqa_graph::GraphStore as _;
        self.synchronize_catalog_registries()?;
        let mut graphs = self.durable.graphs.write();
        if graphs.contains_key(name) {
            return Ok(false);
        }
        let mut candidate = uqa_graph::MemoryGraphStore::new();
        candidate.create_graph(name);
        self.persist_graph_candidate(name, &candidate)?;
        graphs.insert(name.to_string(), candidate);
        drop(graphs);
        self.note_catalog_registry_changed();
        Ok(true)
    }

    /// Drop a named graph. No-op when the graph is missing.
    pub fn drop_graph(&self, name: &str) -> StorageBackendResult<bool> {
        self.with_implicit_storage_transaction(|engine| engine.drop_graph_inner(name))
    }

    fn drop_graph_inner(&self, name: &str) -> StorageBackendResult<bool> {
        self.synchronize_catalog_registries()?;
        let mut graphs = self.durable.graphs.write();
        if !graphs.contains_key(name) {
            return Ok(false);
        }
        if let Some(catalog) = self.storage.catalog.as_ref() {
            catalog.drop_named_graph_data(name)?;
        }
        graphs.remove(name);
        self.durable
            .path_indexes
            .write()
            .retain(|key, _| !key.starts_with(&format!("{name}::")));
        self.note_catalog_registry_changed();
        Ok(true)
    }

    /// Return every named graph registered on this engine in sorted order.
    pub fn list_graphs(&self) -> StorageBackendResult<Vec<String>> {
        self.synchronize_catalog_registries()?;
        Ok(self.durable.graphs.read().keys().cloned().collect())
    }

    /// Return `true` when a graph with `name` is registered.
    pub fn has_graph(&self, name: &str) -> StorageBackendResult<bool> {
        self.synchronize_catalog_registries()?;
        Ok(self.durable.graphs.read().contains_key(name))
    }

    /// Every named graph with its `ag_label` entries, read under one catalog
    /// lock so catalog relations that mirror graphs do not re-read the
    /// registry once per graph.
    pub fn graph_label_catalog(
        &self,
    ) -> StorageBackendResult<Vec<(String, Vec<uqa_graph::GraphLabelInfo>)>> {
        self.synchronize_catalog_registries()?;
        let graphs = self.durable.graphs.read();
        graphs
            .iter()
            .map(|(name, store)| {
                store
                    .graph_labels(name)
                    .map(|labels| (name.clone(), labels))
                    .map_err(graph_store_error)
            })
            .collect()
    }

    /// The `ag_label` entries of a named graph: the two AGE default labels
    /// followed by the user labels in label-id order. `None` when the graph
    /// does not exist.
    pub fn list_graph_labels(
        &self,
        graph: &str,
    ) -> StorageBackendResult<Option<Vec<uqa_graph::GraphLabelInfo>>> {
        self.synchronize_catalog_registries()?;
        let graphs = self.durable.graphs.read();
        let Some(store) = graphs.get(graph) else {
            return Ok(None);
        };
        store
            .graph_labels(graph)
            .map(Some)
            .map_err(graph_store_error)
    }

    /// Register an empty vertex or edge label in a named graph
    /// (`create_vlabel` / `create_elabel`). Returns `false` when a label of
    /// that name already exists in the graph and fails when the graph does
    /// not exist.
    pub fn create_graph_label(
        &self,
        graph: &str,
        label: &str,
        kind: uqa_graph::LabelKind,
    ) -> StorageBackendResult<bool> {
        self.with_implicit_storage_transaction(move |engine| {
            engine.create_graph_label_inner(graph, label, kind)
        })
    }

    fn create_graph_label_inner(
        &self,
        graph: &str,
        label: &str,
        kind: uqa_graph::LabelKind,
    ) -> StorageBackendResult<bool> {
        self.synchronize_catalog_registries()?;
        let mut graphs = self.durable.graphs.write();
        let Some(store) = graphs.get(graph) else {
            return Err(super::StorageBackendError::Other(format!(
                "graph `{graph}` does not exist"
            )));
        };
        let mut candidate = store.clone();
        let created = candidate
            .create_label(graph, label, kind)
            .map_err(graph_store_error)?
            .is_some();
        if !created {
            return Ok(false);
        }
        self.persist_graph_candidate(graph, &candidate)?;
        graphs.insert(graph.to_string(), candidate);
        drop(graphs);
        self.note_catalog_registry_changed();
        Ok(true)
    }

    /// Drop a user label and every entity carrying it from a named graph
    /// (`drop_label`). Returns `false` when the label is not registered and
    /// fails when the graph does not exist.
    pub fn drop_graph_label(&self, graph: &str, label: &str) -> StorageBackendResult<bool> {
        self.with_implicit_storage_transaction(move |engine| {
            engine.drop_graph_label_inner(graph, label)
        })
    }

    fn drop_graph_label_inner(&self, graph: &str, label: &str) -> StorageBackendResult<bool> {
        self.synchronize_catalog_registries()?;
        let mut graphs = self.durable.graphs.write();
        let Some(store) = graphs.get(graph) else {
            return Err(super::StorageBackendError::Other(format!(
                "graph `{graph}` does not exist"
            )));
        };
        let mut candidate = store.clone();
        let dropped = candidate
            .drop_label(graph, label)
            .map_err(graph_store_error)?
            .is_some();
        if !dropped {
            return Ok(false);
        }
        self.persist_graph_candidate(graph, &candidate)?;
        graphs.insert(graph.to_string(), candidate);
        self.invalidate_graph_path_indexes(graph);
        drop(graphs);
        self.note_catalog_registry_changed();
        Ok(true)
    }

    /// Rename a named graph (`alter_graph(..., 'RENAME', ...)`). Returns
    /// `false` when `from` does not exist and fails when `to` is already a
    /// graph.
    pub fn rename_graph(&self, from: &str, to: &str) -> StorageBackendResult<bool> {
        self.with_implicit_storage_transaction(move |engine| engine.rename_graph_inner(from, to))
    }

    fn rename_graph_inner(&self, from: &str, to: &str) -> StorageBackendResult<bool> {
        self.synchronize_catalog_registries()?;
        let mut graphs = self.durable.graphs.write();
        let Some(store) = graphs.get(from) else {
            return Ok(false);
        };
        if from == to {
            return Ok(true);
        }
        if graphs.contains_key(to) {
            return Err(super::StorageBackendError::Other(format!(
                "graph `{to}` already exists"
            )));
        }
        let mut candidate = store.clone();
        candidate
            .rename_graph(from, to)
            .map_err(graph_store_error)?;
        self.persist_graph_candidate(to, &candidate)?;
        if let Some(catalog) = self.storage.catalog.as_ref() {
            catalog.drop_named_graph_data(from)?;
        }
        graphs.remove(from);
        graphs.insert(to.to_string(), candidate);
        self.invalidate_graph_path_indexes(from);
        drop(graphs);
        self.note_catalog_registry_changed();
        Ok(true)
    }

    /// Insert a vertex into a named graph, creating the graph if needed.
    pub fn add_graph_vertex(
        &self,
        vertex: uqa_core::Vertex,
        graph: &str,
    ) -> StorageBackendResult<()> {
        self.with_implicit_storage_transaction(move |engine| {
            engine.add_graph_vertex_inner(vertex, graph)
        })
    }

    fn add_graph_vertex_inner(
        &self,
        vertex: uqa_core::Vertex,
        graph: &str,
    ) -> StorageBackendResult<()> {
        use uqa_graph::GraphStore as _;
        self.synchronize_catalog_registries()?;
        let mut graphs = self.durable.graphs.write();
        let mut candidate = graphs.get(graph).cloned().unwrap_or_default();
        if !candidate.has_graph(graph) {
            candidate.create_graph(graph);
        }
        candidate
            .add_vertex(vertex, graph)
            .map_err(graph_store_error)?;
        self.persist_graph_candidate(graph, &candidate)?;
        graphs.insert(graph.to_string(), candidate);
        self.invalidate_graph_path_indexes(graph);
        drop(graphs);
        self.note_catalog_registry_changed();
        Ok(())
    }

    /// Insert an edge into a named graph, creating the graph if needed.
    pub fn add_graph_edge(&self, edge: uqa_core::Edge, graph: &str) -> StorageBackendResult<()> {
        self.with_implicit_storage_transaction(move |engine| {
            engine.add_graph_edge_inner(edge, graph)
        })
    }

    fn add_graph_edge_inner(&self, edge: uqa_core::Edge, graph: &str) -> StorageBackendResult<()> {
        use uqa_graph::GraphStore as _;
        self.synchronize_catalog_registries()?;
        let mut graphs = self.durable.graphs.write();
        let mut candidate = graphs.get(graph).cloned().unwrap_or_default();
        if !candidate.has_graph(graph) {
            candidate.create_graph(graph);
        }
        candidate.add_edge(edge, graph).map_err(graph_store_error)?;
        self.persist_graph_candidate(graph, &candidate)?;
        graphs.insert(graph.to_string(), candidate);
        self.invalidate_graph_path_indexes(graph);
        drop(graphs);
        self.note_catalog_registry_changed();
        Ok(())
    }

    /// Apply a [`uqa_graph::GraphDelta`] to a named graph as one atomic batch
    /// of vertex and edge additions or removals.
    pub fn apply_graph_delta(
        &self,
        graph: &str,
        delta: &uqa_graph::GraphDelta,
    ) -> StorageBackendResult<()> {
        self.with_implicit_storage_transaction(|engine| {
            engine.apply_graph_delta_inner(graph, delta)
        })
    }

    fn apply_graph_delta_inner(
        &self,
        graph: &str,
        delta: &uqa_graph::GraphDelta,
    ) -> StorageBackendResult<()> {
        use uqa_graph::DeltaOp;
        use uqa_graph::GraphStore as _;
        self.synchronize_catalog_registries()?;
        let mut graphs = self.durable.graphs.write();
        let mut candidate = graphs.get(graph).cloned().unwrap_or_default();
        if !candidate.has_graph(graph) {
            candidate.create_graph(graph);
        }
        for op in delta.ops() {
            match op {
                DeltaOp::AddVertex(v) => candidate.add_vertex(v.clone(), graph),
                DeltaOp::RemoveVertex(id) => candidate.remove_vertex(*id, graph),
                DeltaOp::AddEdge(e) => candidate.add_edge(e.clone(), graph),
                DeltaOp::RemoveEdge(id) => candidate.remove_edge(*id, graph),
            }
            .map_err(graph_store_error)?;
        }
        self.persist_graph_candidate(graph, &candidate)?;
        graphs.insert(graph.to_string(), candidate);
        self.invalidate_graph_path_indexes(graph);
        drop(graphs);
        self.note_catalog_registry_changed();
        Ok(())
    }

    /// Build (or replace) a path index for `graph` keyed by `name`.
    /// `label_sequences` is the set of label sequences to materialize; each
    /// sequence becomes a hash-friendly direct lookup for RPQ.
    pub fn build_path_index(
        &self,
        name: &str,
        graph: &str,
        label_sequences: &[Vec<String>],
    ) -> StorageBackendResult<bool> {
        self.with_implicit_storage_transaction(|engine| {
            engine.build_path_index_inner(name, graph, label_sequences)
        })
    }

    fn build_path_index_inner(
        &self,
        name: &str,
        graph: &str,
        label_sequences: &[Vec<String>],
    ) -> StorageBackendResult<bool> {
        self.synchronize_catalog_registries()?;
        let key = format!("{graph}::{name}");
        let idx = {
            let graphs = self.durable.graphs.read();
            let Some(store) = graphs.get(graph) else {
                return Ok(false);
            };
            uqa_graph::PathIndex::build(store, graph, label_sequences).map_err(graph_store_error)?
        };
        if let Some(catalog) = self.storage.catalog.as_ref() {
            let seq_json = serde_json::to_string(label_sequences)?;
            catalog.save_path_index(&key, &seq_json)?;
        }
        self.durable.path_indexes.write().insert(key, idx);
        self.note_catalog_registry_changed();
        Ok(true)
    }

    /// Drop a path index by `(graph, name)`. Return `true` when one existed.
    pub fn drop_path_index(&self, name: &str, graph: &str) -> StorageBackendResult<bool> {
        self.with_implicit_storage_transaction(|engine| engine.drop_path_index_inner(name, graph))
    }

    fn drop_path_index_inner(&self, name: &str, graph: &str) -> StorageBackendResult<bool> {
        self.synchronize_catalog_registries()?;
        let key = format!("{graph}::{name}");
        if !self.durable.path_indexes.read().contains_key(&key) {
            return Ok(false);
        }
        if let Some(catalog) = self.storage.catalog.as_ref() {
            catalog.drop_path_index(&key)?;
        }
        let removed = self.durable.path_indexes.write().remove(&key).is_some();
        if removed {
            self.note_catalog_registry_changed();
        }
        Ok(removed)
    }

    /// Look up a path index by `(graph, name)`. Return a clone so the caller
    /// is not tied to the engine's lock.
    pub fn get_path_index(
        &self,
        name: &str,
        graph: &str,
    ) -> StorageBackendResult<Option<uqa_graph::PathIndex>> {
        self.synchronize_catalog_registries()?;
        let key = format!("{graph}::{name}");
        Ok(self.durable.path_indexes.read().get(&key).cloned())
    }

    /// Sorted list of registered path index keys. Each key has the
    /// shape `<graph>::<name>` so the caller can split as needed.
    pub fn list_path_indexes(&self) -> StorageBackendResult<Vec<String>> {
        self.synchronize_catalog_registries()?;
        Ok(self.durable.path_indexes.read().keys().cloned().collect())
    }

    /// Read-only borrow of a named graph for ad-hoc query construction
    /// outside the SQL function path. Returns `None` when the graph
    /// is unknown.
    pub fn graph_with<R>(
        &self,
        name: &str,
        f: impl FnOnce(&uqa_graph::MemoryGraphStore) -> R,
    ) -> StorageBackendResult<Option<R>> {
        self.synchronize_catalog_registries()?;
        let graphs = self.durable.graphs.read();
        Ok(graphs.get(name).map(f))
    }

    /// Mutable borrow of a named graph for vertex / edge insertion.
    pub fn graph_with_mut<R>(
        &self,
        name: &str,
        f: impl FnOnce(&mut uqa_graph::MemoryGraphStore) -> uqa_graph::GraphStoreResult<R>,
    ) -> StorageBackendResult<Option<R>> {
        self.with_implicit_storage_transaction(move |engine| engine.graph_with_mut_inner(name, f))
    }

    fn graph_with_mut_inner<R>(
        &self,
        name: &str,
        f: impl FnOnce(&mut uqa_graph::MemoryGraphStore) -> uqa_graph::GraphStoreResult<R>,
    ) -> StorageBackendResult<Option<R>> {
        self.synchronize_catalog_registries()?;
        let mut graphs = self.durable.graphs.write();
        let Some(store) = graphs.get(name) else {
            return Ok(None);
        };
        let mut candidate = store.clone();
        let result = f(&mut candidate).map_err(graph_store_error)?;
        self.persist_graph_candidate(name, &candidate)?;
        graphs.insert(name.to_string(), candidate);
        self.invalidate_graph_path_indexes(name);
        drop(graphs);
        self.note_catalog_registry_changed();
        Ok(Some(result))
    }

    /// Run a Cypher query against a named graph and return the
    /// `(columns, rows)` projected by the query's `RETURN` clause (or
    /// empty vectors when the query has no `RETURN`).
    ///
    /// This wires the full `CREATE` / `MERGE` / `SET` / `DELETE` /
    /// `UNWIND` surface through to the in-memory store. The named graph is
    /// auto-created on first use.
    pub fn run_cypher(
        &self,
        graph: &str,
        query: &str,
        params: BTreeMap<String, Value>,
    ) -> Result<(Vec<String>, Vec<uqa_graph::cypher::ResultRow>), uqa_graph::cypher::CypherError>
    {
        self.with_implicit_mapped_transaction(
            move |engine| engine.run_cypher_inner(graph, query, params),
            uqa_graph::cypher::CypherError::Storage,
        )
    }

    fn run_cypher_inner(
        &self,
        graph: &str,
        query: &str,
        params: BTreeMap<String, Value>,
    ) -> Result<(Vec<String>, Vec<uqa_graph::cypher::ResultRow>), uqa_graph::cypher::CypherError>
    {
        use uqa_graph::cypher::{parse_cypher, CypherWriter};
        use uqa_graph::GraphStore as _;
        self.synchronize_catalog_registries()
            .map_err(|err| uqa_graph::cypher::CypherError::Storage(err.to_string()))?;
        let q = parse_cypher(query)?;
        let mut graphs = self.durable.graphs.write();
        let existed = graphs.contains_key(graph);
        let mut candidate = graphs.get(graph).cloned().unwrap_or_default();
        if !candidate.has_graph(graph) {
            candidate.create_graph(graph);
        }
        let mutates = q.clauses.iter().any(|clause| {
            matches!(
                clause,
                uqa_graph::cypher::CypherClause::Create(_)
                    | uqa_graph::cypher::CypherClause::Merge(_)
                    | uqa_graph::cypher::CypherClause::Set(_)
                    | uqa_graph::cypher::CypherClause::Delete(_)
            )
        });
        let result = {
            // Ensure the named partition exists inside the store as
            // well. The outer map only owns the store; create_graph
            // populates the store's own partition registry that
            // mutations key off of.
            let mut writer = CypherWriter::new(&mut candidate, graph).with_params(params);
            writer.execute(&q)?
        };
        if mutates || !existed {
            self.persist_graph_candidate(graph, &candidate)
                .map_err(|err| uqa_graph::cypher::CypherError::Storage(err.to_string()))?;
            graphs.insert(graph.to_string(), candidate);
            self.invalidate_graph_path_indexes(graph);
            drop(graphs);
            self.note_catalog_registry_changed();
        }
        Ok(result)
    }

    fn persist_graph_candidate(
        &self,
        graph: &str,
        store: &uqa_graph::MemoryGraphStore,
    ) -> StorageBackendResult<()> {
        use uqa_graph::GraphStore as _;
        let vertex_ids = store
            .vertex_ids_in_graph(graph)
            .map_err(graph_store_error)?;
        for edge in store.edges_in_graph(graph).map_err(graph_store_error)? {
            if !vertex_ids.contains(&edge.source_id) || !vertex_ids.contains(&edge.target_id) {
                return Err(super::StorageBackendError::Other(format!(
                    "graph `{graph}` edge {} references missing endpoint {} -> {}",
                    edge.edge_id, edge.source_id, edge.target_id
                )));
            }
        }
        let Some(catalog) = self.storage.catalog.as_ref() else {
            return Ok(());
        };
        let vertices = store
            .vertices_in_graph(graph)
            .map_err(graph_store_error)?
            .into_iter()
            .map(|vertex| {
                Ok(GraphVertexRow {
                    vertex_id: vertex.vertex_id,
                    label: vertex.label,
                    properties_json: serde_json::to_string(&vertex.properties)?,
                })
            })
            .collect::<StorageBackendResult<Vec<_>>>()?;
        let edges = store
            .edges_in_graph(graph)
            .map_err(graph_store_error)?
            .into_iter()
            .map(|edge| {
                Ok(EdgeRow {
                    edge_id: edge.edge_id,
                    source_id: edge.source_id,
                    target_id: edge.target_id,
                    label: edge.label,
                    properties_json: serde_json::to_string(&edge.properties)?,
                })
            })
            .collect::<StorageBackendResult<Vec<_>>>()?;
        let snapshot = GraphSnapshot {
            vertices,
            edges,
            label_registry_json: serde_json::to_string(&store.label_registry(graph))?,
        };
        catalog.replace_named_graph(graph, &snapshot)
    }

    fn invalidate_graph_path_indexes(&self, graph: &str) {
        self.durable
            .path_indexes
            .write()
            .retain(|key, _| !key.starts_with(&format!("{graph}::")));
    }
}
