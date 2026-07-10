//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{BTreeMap, Engine, Value, GRAPH_LABELS_METADATA_PREFIX};

impl Engine {
    pub fn create_graph(&self, name: impl Into<String>) {
        let name = name.into();
        let mut graphs = self.graphs.write();
        graphs.entry(name.clone()).or_default();
        drop(graphs);
        if let Some(catalog) = self.catalog.as_ref() {
            let _ = catalog.save_named_graph(&name);
        }
    }

    /// Drop a named graph. No-op when the graph is missing.
    pub fn drop_graph(&self, name: &str) {
        self.graphs.write().remove(name);
        if let Some(catalog) = self.catalog.as_ref() {
            let _ = catalog.drop_named_graph(name);
            // Vertex / edge rows survive in the global tables until
            // every graph has detached them; sweep the orphans now so
            // the catalog stays in sync with the in-memory view.
            let _ = catalog.purge_orphan_graph_entities();
            // Clear the persisted AGE label registry so a re-created
            // graph starts from label id 3 / sequence 1 again.
            let _ = catalog.set_metadata(&format!("{GRAPH_LABELS_METADATA_PREFIX}{name}"), "");
        }
    }

    /// Sorted list of every named graph registered on this engine.
    /// Mirrors the canonical UQA implementation's `Engine.list_graphs`.
    pub fn list_graphs(&self) -> Vec<String> {
        self.graphs.read().keys().cloned().collect()
    }

    /// Return `true` when a graph with `name` is registered.
    /// Mirrors the canonical UQA implementation's `Engine.has_graph`.
    pub fn has_graph(&self, name: &str) -> bool {
        self.graphs.read().contains_key(name)
    }

    /// Insert a vertex into a named graph. Auto-creates the graph if
    /// missing. Mirrors the canonical UQA implementation's `Engine.add_graph_vertex`.
    pub fn add_graph_vertex(&self, vertex: uqa_core::Vertex, graph: &str) {
        use uqa_graph::GraphStore as _;
        let vertex_id = vertex.vertex_id;
        // Snapshot the persistable shape (label + properties JSON)
        // before moving the value into the in-memory store so the
        // catalog write below sees the exact same data.
        let persist = self.catalog.as_ref().and_then(|_| {
            serde_json::to_string(&vertex.properties)
                .ok()
                .map(|p| (vertex.label.clone(), p))
        });
        {
            let mut graphs = self.graphs.write();
            let store = graphs.entry(graph.to_string()).or_default();
            if !store.has_graph(graph) {
                store.create_graph(graph);
            }
            store.add_vertex(vertex, graph);
        }
        if let Some(catalog) = self.catalog.as_ref() {
            let _ = catalog.save_named_graph(graph);
            if let Some((label, props_json)) = persist {
                let _ = catalog.save_vertex(vertex_id, &label, &props_json);
                let _ = catalog.save_graph_membership("vertex", vertex_id, graph);
            }
        }
    }

    /// Insert an edge into a named graph. Auto-creates the graph if
    /// missing. Mirrors the canonical UQA implementation's `Engine.add_graph_edge`.
    pub fn add_graph_edge(&self, edge: uqa_core::Edge, graph: &str) {
        use uqa_graph::GraphStore as _;
        let edge_id = edge.edge_id;
        let edge_source = edge.source_id;
        let edge_target = edge.target_id;
        let persist = self.catalog.as_ref().and_then(|_| {
            serde_json::to_string(&edge.properties)
                .ok()
                .map(|p| (edge.label.clone(), p))
        });
        {
            let mut graphs = self.graphs.write();
            let store = graphs.entry(graph.to_string()).or_default();
            if !store.has_graph(graph) {
                store.create_graph(graph);
            }
            store.add_edge(edge, graph);
        }
        if let Some(catalog) = self.catalog.as_ref() {
            let _ = catalog.save_named_graph(graph);
            if let Some((label, props_json)) = persist {
                let _ = catalog.save_edge(edge_id, edge_source, edge_target, &label, &props_json);
                let _ = catalog.save_graph_membership("edge", edge_id, graph);
            }
        }
    }

    /// Apply a [`uqa_graph::GraphDelta`] to a named graph as a single
    /// atomic batch of `add/remove vertex/edge` ops. Mirrors the canonical UQA implementation's
    /// `Engine.apply_graph_delta`.
    pub fn apply_graph_delta(&self, graph: &str, delta: &uqa_graph::GraphDelta) {
        use uqa_graph::DeltaOp;
        use uqa_graph::GraphStore as _;
        let mut graphs = self.graphs.write();
        let store = graphs.entry(graph.to_string()).or_default();
        if !store.has_graph(graph) {
            store.create_graph(graph);
        }
        for op in delta.ops() {
            match op {
                DeltaOp::AddVertex(v) => store.add_vertex(v.clone(), graph),
                DeltaOp::RemoveVertex(id) => store.remove_vertex(*id, graph),
                DeltaOp::AddEdge(e) => store.add_edge(e.clone(), graph),
                DeltaOp::RemoveEdge(id) => store.remove_edge(*id, graph),
            }
        }
        drop(graphs);
        if let Some(catalog) = self.catalog.as_ref() {
            let _ = catalog.save_named_graph(graph);
            let mut needs_purge = false;
            for op in delta.ops() {
                match op {
                    DeltaOp::AddVertex(v) => {
                        if let Ok(props_json) = serde_json::to_string(&v.properties) {
                            let _ = catalog.save_vertex(v.vertex_id, &v.label, &props_json);
                            let _ = catalog.save_graph_membership("vertex", v.vertex_id, graph);
                        }
                    }
                    DeltaOp::RemoveVertex(id) => {
                        let _ = catalog.delete_graph_membership("vertex", *id, graph);
                        needs_purge = true;
                    }
                    DeltaOp::AddEdge(e) => {
                        if let Ok(props_json) = serde_json::to_string(&e.properties) {
                            let _ = catalog.save_edge(
                                e.edge_id,
                                e.source_id,
                                e.target_id,
                                &e.label,
                                &props_json,
                            );
                            let _ = catalog.save_graph_membership("edge", e.edge_id, graph);
                        }
                    }
                    DeltaOp::RemoveEdge(id) => {
                        let _ = catalog.delete_graph_membership("edge", *id, graph);
                        needs_purge = true;
                    }
                }
            }
            if needs_purge {
                // Vertex / edge rows survive only while at least one
                // graph still references them via `_graph_membership`.
                let _ = catalog.purge_orphan_graph_entities();
            }
        }
        // Invalidate any cached path indexes for this graph: a path
        // index is built against a snapshot, so the safe move is to
        // drop them and let the caller rebuild on demand.
        self.path_indexes
            .write()
            .retain(|key, _| !key.starts_with(&format!("{graph}::")));
    }

    /// Build (or replace) a path index for `graph` keyed by `name`.
    /// `label_sequences` is the set of label sequences to materialise;
    /// each sequence becomes a hash-friendly direct lookup for RPQ.
    /// Mirrors the canonical UQA implementation's `Engine.build_path_index`.
    pub fn build_path_index(&self, name: &str, graph: &str, label_sequences: &[Vec<String>]) {
        let key = format!("{graph}::{name}");
        let idx = {
            let graphs = self.graphs.read();
            let Some(store) = graphs.get(graph) else {
                return;
            };
            uqa_graph::PathIndex::build(store, graph, label_sequences)
        };
        self.path_indexes.write().insert(key.clone(), idx);
        if let Some(catalog) = self.catalog.as_ref() {
            let seq_json = serde_json::to_string(label_sequences).unwrap_or_else(|_| "[]".into());
            let _ = catalog.save_path_index(&key, &seq_json);
        }
    }

    /// Drop a path index by `(graph, name)`. Returns `true` when an
    /// index was removed. Mirrors the canonical UQA implementation's `Engine.drop_path_index`.
    pub fn drop_path_index(&self, name: &str, graph: &str) -> bool {
        let key = format!("{graph}::{name}");
        let removed = self.path_indexes.write().remove(&key).is_some();
        if removed {
            if let Some(catalog) = self.catalog.as_ref() {
                let _ = catalog.drop_path_index(&key);
            }
        }
        removed
    }

    /// Look up a path index by `(graph, name)`. Returns a clone so the
    /// caller is not tied to the engine's lock. Mirrors the canonical UQA implementation's
    /// `Engine.get_path_index`.
    pub fn get_path_index(&self, name: &str, graph: &str) -> Option<uqa_graph::PathIndex> {
        let key = format!("{graph}::{name}");
        self.path_indexes.read().get(&key).cloned()
    }

    /// Sorted list of registered path index keys. Each key has the
    /// shape `<graph>::<name>` so the caller can split as needed.
    pub fn list_path_indexes(&self) -> Vec<String> {
        self.path_indexes.read().keys().cloned().collect()
    }

    /// Read-only borrow of a named graph for ad-hoc query construction
    /// outside the SQL function path. Returns `None` when the graph
    /// is unknown.
    pub fn graph_with<R>(
        &self,
        name: &str,
        f: impl FnOnce(&uqa_graph::MemoryGraphStore) -> R,
    ) -> Option<R> {
        let graphs = self.graphs.read();
        graphs.get(name).map(f)
    }

    /// Mutable borrow of a named graph for vertex / edge insertion.
    pub fn graph_with_mut<R>(
        &self,
        name: &str,
        f: impl FnOnce(&mut uqa_graph::MemoryGraphStore) -> R,
    ) -> Option<R> {
        let result = {
            let mut graphs = self.graphs.write();
            graphs.get_mut(name).map(f)
        };
        if result.is_some() {
            self.resync_graph_to_catalog(name);
        }
        result
    }

    /// Run a Cypher query against a named graph and return the
    /// `(columns, rows)` projected by the query's `RETURN` clause (or
    /// empty vectors when the query has no `RETURN`).
    ///
    /// This wires the full `CREATE` / `MERGE` / `SET` / `DELETE` /
    /// `UNWIND` surface through to the in-memory store. The graph is
    /// auto-created on first use, mirroring the canonical UQA implementation's
    /// `CypherCompiler.execute` behaviour.
    pub fn run_cypher(
        &self,
        graph: &str,
        query: &str,
        params: BTreeMap<String, Value>,
    ) -> Result<(Vec<String>, Vec<uqa_graph::cypher::ResultRow>), uqa_graph::cypher::CypherError>
    {
        use uqa_graph::cypher::{parse_cypher, CypherWriter};
        use uqa_graph::GraphStore as _;
        let q = parse_cypher(query)?;
        let result = {
            let mut graphs = self.graphs.write();
            let store = graphs.entry(graph.to_string()).or_default();
            // Ensure the named partition exists inside the store as
            // well. The outer map only owns the store; create_graph
            // populates the store's own partition registry that
            // mutations key off of.
            if !store.has_graph(graph) {
                store.create_graph(graph);
            }
            let mut writer = CypherWriter::new(store, graph).with_params(params);
            writer.execute(&q)?
        };
        self.resync_graph_to_catalog(graph);
        Ok(result)
    }

    /// Mirror the in-memory graph back to the catalog after a write.
    /// Cypher / `graph_with_mut` callers can edit the store directly,
    /// so the simplest correct strategy is a full resync of `graph`'s
    /// membership rows: drop every membership for the graph, re-insert
    /// each vertex / edge currently in the partition, then garbage
    /// collect any vertex / edge that fell out of every other graph's
    /// membership too.
    fn resync_graph_to_catalog(&self, graph: &str) {
        use uqa_graph::GraphStore as _;
        let Some(catalog) = self.catalog.as_ref() else {
            return;
        };
        let graphs = self.graphs.read();
        let Some(store) = graphs.get(graph) else {
            return;
        };
        let _ = catalog.save_named_graph(graph);
        let _ = catalog.delete_graph_membership_for_graph(graph);
        for vertex in store.vertices_in_graph(graph) {
            if let Ok(props_json) = serde_json::to_string(&vertex.properties) {
                let _ = catalog.save_vertex(vertex.vertex_id, &vertex.label, &props_json);
                let _ = catalog.save_graph_membership("vertex", vertex.vertex_id, graph);
            }
        }
        for edge in store.edges_in_graph(graph) {
            if let Ok(props_json) = serde_json::to_string(&edge.properties) {
                let _ = catalog.save_edge(
                    edge.edge_id,
                    edge.source_id,
                    edge.target_id,
                    &edge.label,
                    &props_json,
                );
                let _ = catalog.save_graph_membership("edge", edge.edge_id, graph);
            }
        }
        let _ = catalog.purge_orphan_graph_entities();
        // Persist the AGE label registry so id allocation stays
        // deterministic across engine restarts even when a label's
        // entities were all deleted.
        if let Ok(json) = serde_json::to_string(&store.label_registry(graph)) {
            let _ = catalog.set_metadata(&format!("{GRAPH_LABELS_METADATA_PREFIX}{graph}"), &json);
        }
    }
}
