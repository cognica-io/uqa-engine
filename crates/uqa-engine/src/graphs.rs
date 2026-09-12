//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{BTreeMap, Engine, RelationIdentity, StorageBackendResult, Value};
use uqa_graph::GraphStore as _;

mod snapshots;

fn graph_store_error(error: impl std::fmt::Display) -> super::StorageBackendError {
    super::StorageBackendError::Other(error.to_string())
}

impl Engine {
    pub fn create_graph(&self, name: impl Into<String>) -> StorageBackendResult<bool> {
        let name = name.into();
        self.with_implicit_graph_transaction(|engine| engine.create_graph_inner(&name))
    }

    fn create_graph_inner(&self, name: &str) -> StorageBackendResult<bool> {
        use uqa_graph::GraphStore as _;
        self.synchronize_catalog_registries()?;
        let mut candidate = self
            .graph_write_candidate(name, true)?
            .expect("create candidate");
        if candidate.has_graph(name).map_err(graph_store_error)? {
            return Ok(false);
        }
        candidate.create_graph(name).map_err(graph_store_error)?;
        let published = self.publish_graph_candidate(candidate)?;
        self.durable
            .graphs
            .write()
            .insert(name.to_string(), published);
        self.note_catalog_registry_changed();
        Ok(true)
    }

    /// Drop a named graph. No-op when the graph is missing.
    pub fn drop_graph(&self, name: &str) -> StorageBackendResult<bool> {
        self.with_implicit_graph_transaction(|engine| engine.drop_graph_inner(name))
    }

    fn drop_graph_inner(&self, name: &str) -> StorageBackendResult<bool> {
        self.synchronize_catalog_registries()?;
        let Some(mut store) = self.graph_write_candidate(name, false)? else {
            return Ok(false);
        };
        let labels = store.graph_labels(name).map_err(graph_store_error)?;
        let label_relations = labels
            .iter()
            .map(|label| RelationIdentity::new(name, &label.name).qualified_name())
            .collect::<Vec<_>>();
        self.drop_views_depending_on_relations(&label_relations)?;
        store.drop_graph(name).map_err(graph_store_error)?;
        self.invalidate_graph_path_indexes(name)?;
        self.publish_graph_candidate(store)?;
        self.durable.graphs.write().remove(name);
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
        Ok(self.visible_graph_handles().keys().cloned().collect())
    }

    /// Return `true` when a graph with `name` is registered.
    pub fn has_graph(&self, name: &str) -> StorageBackendResult<bool> {
        self.synchronize_catalog_registries()?;
        Ok(self.graph_handle_in_execution(name).is_some())
    }

    /// Every named graph with its `ag_label` entries, read under one catalog
    /// lock so catalog relations that mirror graphs do not re-read the
    /// registry once per graph.
    pub fn graph_label_catalog(
        &self,
    ) -> StorageBackendResult<Vec<(String, Vec<uqa_graph::GraphLabelInfo>)>> {
        self.synchronize_catalog_registries()?;
        let graphs = self.visible_graph_handles();
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

    /// The surviving `ag_label` entries of a named graph in label-id order.
    /// `None` when the graph does not exist.
    pub fn list_graph_labels(
        &self,
        graph: &str,
    ) -> StorageBackendResult<Option<Vec<uqa_graph::GraphLabelInfo>>> {
        self.synchronize_catalog_registries()?;
        let Some(store) = self.graph_handle_in_execution(graph) else {
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
        self.with_implicit_graph_transaction(move |engine| {
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
        let Some(mut candidate) = self.graph_write_candidate(graph, false)? else {
            return Err(super::StorageBackendError::Other(format!(
                "graph `{graph}` does not exist"
            )));
        };
        let created = candidate
            .create_label(graph, label, kind)
            .map_err(graph_store_error)?
            .is_some();
        if !created {
            return Ok(false);
        }
        let published = self.publish_graph_candidate(candidate)?;
        self.durable
            .graphs
            .write()
            .insert(graph.to_string(), published);
        self.note_catalog_registry_changed();
        Ok(true)
    }

    /// Drop a label and every entity carrying it from a named graph
    /// (`drop_label`). Incident edge rows survive vertex-label removal like
    /// AGE's `DROP TABLE`. Returns `false` when the label is not registered
    /// and fails when the graph does not exist.
    pub fn drop_graph_label(&self, graph: &str, label: &str) -> StorageBackendResult<bool> {
        self.with_implicit_graph_transaction(move |engine| {
            engine.drop_graph_label_inner(graph, label)
        })
    }

    /// Stored views whose exact relation binding prevents a label relation from being dropped.
    pub(crate) fn graph_label_relation_dependents(
        &self,
        graph: &str,
        label: &str,
    ) -> StorageBackendResult<Vec<String>> {
        self.synchronize_catalog_registries()?;
        let relation_name = format!(
            "{}.{}",
            uqa_sql::expr::quote_ident(graph),
            uqa_sql::expr::quote_ident(label)
        );
        self.views_depending_on_relation(&relation_name)
    }

    fn drop_graph_label_inner(&self, graph: &str, label: &str) -> StorageBackendResult<bool> {
        let dependent_views = self.graph_label_relation_dependents(graph, label)?;
        if !dependent_views.is_empty() {
            return Err(super::StorageBackendError::Other(format!(
                "cannot drop label `{}.{}`: dependent view(s) `{}` still reference it",
                uqa_sql::expr::quote_ident(graph),
                uqa_sql::expr::quote_ident(label),
                dependent_views.join("`, `")
            )));
        }
        let Some(mut candidate) = self.graph_write_candidate(graph, false)? else {
            return Err(super::StorageBackendError::Other(format!(
                "graph `{graph}` does not exist"
            )));
        };
        let dropped = candidate
            .drop_label(graph, label)
            .map_err(graph_store_error)?
            .is_some();
        if !dropped {
            return Ok(false);
        }
        self.invalidate_graph_path_indexes(graph)?;
        let published = self.publish_graph_candidate(candidate)?;
        self.durable
            .graphs
            .write()
            .insert(graph.to_string(), published);
        self.note_catalog_registry_changed();
        Ok(true)
    }

    /// Rename a named graph (`alter_graph(..., 'RENAME', ...)`). Returns
    /// `false` when `from` does not exist and fails when `to` is already a
    /// graph.
    pub fn rename_graph(&self, from: &str, to: &str) -> StorageBackendResult<bool> {
        self.with_implicit_graph_transaction(move |engine| engine.rename_graph_inner(from, to))
    }

    fn rename_graph_inner(&self, from: &str, to: &str) -> StorageBackendResult<bool> {
        self.synchronize_catalog_registries()?;
        let Some(mut candidate) = self.graph_write_candidate(from, false)? else {
            return Ok(false);
        };
        if from == to {
            return Ok(true);
        }
        if self.has_graph(to)? {
            return Err(super::StorageBackendError::Other(format!(
                "graph `{to}` already exists"
            )));
        }
        let labels = candidate.graph_labels(from).map_err(graph_store_error)?;
        candidate
            .rename_graph(from, to)
            .map_err(graph_store_error)?;
        let replacements = labels
            .into_iter()
            .map(|label| {
                (
                    RelationIdentity::new(from, &label.name),
                    RelationIdentity::new(to, label.name),
                )
            })
            .collect::<BTreeMap<_, _>>();
        self.rewrite_view_relation_references(&replacements)?;
        self.invalidate_graph_path_indexes(from)?;
        let published = self.publish_graph_candidate(candidate)?;
        let mut graphs = self.durable.graphs.write();
        graphs.remove(from);
        graphs.insert(to.to_string(), published);
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
        self.with_implicit_graph_transaction(move |engine| {
            engine.add_graph_vertex_inner(vertex, graph)
        })
    }

    fn add_graph_vertex_inner(
        &self,
        vertex: uqa_core::Vertex,
        graph: &str,
    ) -> StorageBackendResult<()> {
        self.mutate_graph(graph, true, |store| store.add_vertex(vertex, graph))
            .map(|_| ())
    }

    /// Insert an edge into a named graph, creating the graph if needed.
    pub fn add_graph_edge(&self, edge: uqa_core::Edge, graph: &str) -> StorageBackendResult<()> {
        self.with_implicit_graph_transaction(move |engine| engine.add_graph_edge_inner(edge, graph))
    }

    fn add_graph_edge_inner(&self, edge: uqa_core::Edge, graph: &str) -> StorageBackendResult<()> {
        self.mutate_graph(graph, true, |store| store.add_edge(edge, graph))
            .map(|_| ())
    }

    /// Apply a [`uqa_graph::GraphDelta`] to a named graph as one atomic batch
    /// of vertex and edge additions or removals.
    pub fn apply_graph_delta(
        &self,
        graph: &str,
        delta: &uqa_graph::GraphDelta,
    ) -> StorageBackendResult<()> {
        self.with_implicit_graph_transaction(|engine| engine.apply_graph_delta_inner(graph, delta))
    }

    fn apply_graph_delta_inner(
        &self,
        graph: &str,
        delta: &uqa_graph::GraphDelta,
    ) -> StorageBackendResult<()> {
        self.mutate_graph(graph, true, |store| {
            for op in delta.ops() {
                match op {
                    uqa_graph::DeltaOp::AddVertex(vertex) => {
                        store.add_vertex(vertex.clone(), graph)
                    }
                    uqa_graph::DeltaOp::RemoveVertex(id) => store.remove_vertex(*id, graph),
                    uqa_graph::DeltaOp::AddEdge(edge) => store.add_edge(edge.clone(), graph),
                    uqa_graph::DeltaOp::RemoveEdge(id) => store.remove_edge(*id, graph),
                }?;
            }
            Ok(())
        })
        .map(|_| ())
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
        self.with_implicit_graph_transaction(|engine| {
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
            match (&self.storage.catalog, &self.storage.backend) {
                (Some(catalog), Some(backend)) => uqa_graph::PathIndex::build_persistent(
                    std::sync::Arc::clone(catalog),
                    std::sync::Arc::clone(backend),
                    &key,
                    graph,
                    label_sequences,
                ),
                (None, None) => uqa_graph::PathIndex::build(store.as_ref(), graph, label_sequences),
                _ => {
                    return Err(graph_store_error(
                        "path-index catalog and backend must share a storage session",
                    ))
                }
            }
            .map_err(graph_store_error)?
        };
        self.durable.path_indexes.write().insert(key, idx);
        self.note_catalog_registry_changed();
        Ok(true)
    }

    /// Drop a path index by `(graph, name)`. Return `true` when one existed.
    pub fn drop_path_index(&self, name: &str, graph: &str) -> StorageBackendResult<bool> {
        self.with_implicit_graph_transaction(|engine| engine.drop_path_index_inner(name, graph))
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
        self.with_graph_read_snapshot(|engine| Ok(engine.path_index_in_execution(name, graph)))
    }

    fn path_index_in_execution(&self, name: &str, graph: &str) -> Option<uqa_graph::PathIndex> {
        let key = format!("{graph}::{name}");
        let index = self.query_catalog_snapshot.as_ref().map_or_else(
            || self.durable.path_indexes.read().get(&key).cloned(),
            |snapshot| snapshot.path_indexes.get(&key).cloned(),
        );
        if self.query_catalog_snapshot.is_some()
            || self.session.state.read().graph_overlay.is_some()
        {
            return index.and_then(|index| {
                self.graph_handle_in_execution(graph)
                    .map(|store| index.with_graph_read_view(store))
            });
        }
        index
    }

    /// Sorted list of registered path index keys. Each key has the
    /// shape `<graph>::<name>` so the caller can split as needed.
    pub fn list_path_indexes(&self) -> StorageBackendResult<Vec<String>> {
        self.synchronize_catalog_registries()?;
        Ok(self.query_catalog_snapshot.as_ref().map_or_else(
            || self.durable.path_indexes.read().keys().cloned().collect(),
            |snapshot| snapshot.path_indexes.keys().cloned().collect(),
        ))
    }

    /// Read-only borrow of a named graph for ad-hoc query construction
    /// outside the SQL function path. Returns `None` when the graph
    /// is unknown.
    pub fn graph_with<R>(
        &self,
        name: &str,
        f: impl FnOnce(&uqa_graph::GraphStoreHandle) -> R,
    ) -> StorageBackendResult<Option<R>> {
        self.graph_handle_with(name, |store| f(store.as_ref()))
    }

    pub(crate) fn graph_handle_with<R>(
        &self,
        name: &str,
        f: impl FnOnce(&std::sync::Arc<uqa_graph::GraphStoreHandle>) -> R,
    ) -> StorageBackendResult<Option<R>> {
        self.with_graph_read_snapshot(|engine| {
            Ok(engine.graph_handle_in_execution(name).as_ref().map(f))
        })
    }

    /// The enclosing query owns the statement gate and physical snapshot.
    /// Parallel operators share that snapshot instead of reacquiring the
    /// coordinator's thread-affine statement gate from a worker thread.
    pub(crate) fn graph_handle_in_execution(
        &self,
        name: &str,
    ) -> Option<std::sync::Arc<uqa_graph::GraphStoreHandle>> {
        if let Some(snapshot) = &self.query_catalog_snapshot {
            snapshot.graphs.get(name).cloned()
        } else if let Some(overlay) = &self.session.state.read().graph_overlay {
            overlay.names.contains(name).then(|| {
                std::sync::Arc::new(uqa_graph::GraphStoreHandle::Persistent(
                    overlay.store.as_ref().clone(),
                ))
            })
        } else {
            self.durable.graphs.read().get(name).cloned()
        }
    }

    pub(crate) fn with_graph_read_snapshot<R>(
        &self,
        f: impl FnOnce(&Self) -> StorageBackendResult<R>,
    ) -> StorageBackendResult<R> {
        let _statement = self.runtime.statement_gate.lock();
        if self.transaction_depth() != 0 {
            self.ensure_transaction_usable()
                .map_err(graph_store_error)?;
            self.prepare_explicit_statement_snapshot(true)
                .map_err(graph_store_error)?;
        }
        let owned = self
            .storage
            .backend
            .as_ref()
            .filter(|backend| !backend.in_transaction())
            .cloned();
        if let Some(backend) = &owned {
            backend.begin_read_transaction()?;
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if owned.is_some() {
                self.refresh_pinned_transaction_snapshot()?;
            } else {
                self.synchronize_catalog_registries()?;
            }
            f(self)
        }));
        let cleanup = owned
            .as_ref()
            .map_or(Ok(()), |backend| backend.rollback_transaction());
        match result {
            Ok(Ok(value)) => {
                cleanup?;
                Ok(value)
            }
            Ok(Err(error)) => match cleanup {
                Ok(()) => Err(error),
                Err(cleanup) => Err(graph_store_error(format!(
                    "graph read failed: {error}; snapshot cleanup failed: {cleanup}"
                ))),
            },
            Err(panic) => std::panic::resume_unwind(panic),
        }
    }

    /// Mutable borrow of a named graph for vertex / edge insertion.
    pub fn graph_with_mut<R>(
        &self,
        name: &str,
        f: impl FnOnce(&mut uqa_graph::GraphStoreHandle) -> uqa_graph::GraphStoreResult<R>,
    ) -> StorageBackendResult<Option<R>> {
        self.with_implicit_graph_transaction(move |engine| engine.graph_with_mut_inner(name, f))
    }

    fn graph_with_mut_inner<R>(
        &self,
        name: &str,
        f: impl FnOnce(&mut uqa_graph::GraphStoreHandle) -> uqa_graph::GraphStoreResult<R>,
    ) -> StorageBackendResult<Option<R>> {
        self.mutate_graph(name, false, f)
    }

    fn mutate_graph<R>(
        &self,
        name: &str,
        create: bool,
        f: impl FnOnce(&mut uqa_graph::GraphStoreHandle) -> uqa_graph::GraphStoreResult<R>,
    ) -> StorageBackendResult<Option<R>> {
        self.synchronize_catalog_registries()?;
        let Some(mut candidate) = self.graph_write_candidate(name, create)? else {
            return Ok(None);
        };
        let result = candidate
            .transaction(|store| {
                if !store.has_graph(name)? {
                    store.create_graph(name)?;
                }
                let result = f(store)?;
                self.invalidate_graph_path_indexes(name)?;
                Ok(result)
            })
            .map_err(graph_store_error)?;
        let published = self.publish_graph_candidate(candidate)?;
        self.durable
            .graphs
            .write()
            .insert(name.to_owned(), published);
        self.note_catalog_registry_changed();
        Ok(Some(result))
    }

    /// Run a Cypher query against a named graph and return the
    /// `(columns, rows)` projected by the query's `RETURN` clause (or
    /// empty vectors when the query has no `RETURN`).
    ///
    /// This wires the full `CREATE` / `MERGE` / `SET` / `DELETE` /
    /// `UNWIND` surface through to the selected storage backend. The named graph is
    /// auto-created on first use.
    pub fn run_cypher(
        &self,
        graph: &str,
        query: &str,
        params: BTreeMap<String, Value>,
    ) -> Result<(Vec<String>, Vec<uqa_graph::cypher::ResultRow>), uqa_graph::cypher::CypherError>
    {
        use uqa_graph::cypher::{CypherError, CypherExecutor};
        let _statement = self.runtime.statement_gate.lock();
        let query = uqa_graph::cypher::parse_cypher(query)?;
        if self.transaction_depth() != 0 {
            self.ensure_transaction_usable()
                .map_err(|error| CypherError::Storage(error.to_string()))?;
            self.prepare_explicit_statement_snapshot(true)
                .map_err(|error| CypherError::Storage(error.to_string()))?;
        }
        let existed = self
            .has_graph(graph)
            .map_err(|error| CypherError::Storage(error.to_string()))?;
        if query.mutates_graph() || !existed {
            self.with_implicit_mapped_transaction(
                |engine| engine.run_cypher_inner(graph, &query, params),
                CypherError::Storage,
            )
        } else {
            self.graph_with(graph, |store| {
                uqa_graph::cypher::validate_default_label_relations(store, graph, &query)?;
                CypherExecutor::new(store, graph)
                    .with_params(params)
                    .execute(&query)
            })
            .map_err(|error| CypherError::Storage(error.to_string()))?
            .ok_or_else(|| CypherError::Storage(format!("graph {graph:?} does not exist")))?
        }
    }

    fn run_cypher_inner(
        &self,
        graph: &str,
        query: &uqa_graph::cypher::CypherQuery,
        params: BTreeMap<String, Value>,
    ) -> Result<(Vec<String>, Vec<uqa_graph::cypher::ResultRow>), uqa_graph::cypher::CypherError>
    {
        use uqa_graph::cypher::{CypherError, CypherWriter};
        self.synchronize_catalog_registries()
            .map_err(|error| CypherError::Storage(error.to_string()))?;
        let mut candidate = self
            .graph_write_candidate(graph, true)
            .map_err(|error| CypherError::Storage(error.to_string()))?
            .expect("create candidate");
        let result = candidate.transaction_mapped(
            |store| {
                if !store.has_graph(graph).map_err(CypherError::from)? {
                    store.create_graph(graph).map_err(CypherError::from)?;
                }
                uqa_graph::cypher::validate_default_label_relations(store, graph, query)?;
                let result = CypherWriter::new(store, graph)
                    .with_params(params)
                    .execute(query)?;
                self.invalidate_graph_path_indexes(graph)
                    .map_err(|error| CypherError::Storage(error.to_string()))?;
                Ok(result)
            },
            CypherError::from,
        )?;
        let published = self
            .publish_graph_candidate(candidate)
            .map_err(|error| CypherError::Storage(error.to_string()))?;
        self.durable
            .graphs
            .write()
            .insert(graph.to_owned(), published);
        self.note_catalog_registry_changed();
        Ok(result)
    }

    fn invalidate_graph_path_indexes(&self, graph: &str) -> StorageBackendResult<()> {
        if let Some(catalog) = &self.storage.catalog {
            for (key, _) in catalog.load_path_indexes()? {
                if key.starts_with(&format!("{graph}::")) {
                    catalog.drop_path_index(&key)?;
                }
            }
        }
        self.durable
            .path_indexes
            .write()
            .retain(|key, _| !key.starts_with(&format!("{graph}::")));
        Ok(())
    }
}
