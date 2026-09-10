//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Standalone `SQLite` graph storage using indexed records directly.
//!
//! The handle does not load a graph on open or retain entity/adjacency maps.
//! Existing per-table schemas and legacy property encodings remain readable.
//! Mutations use physical transactions/savepoints; multi-read queries pin one
//! storage snapshot. Owned query results belong to the caller, not the store.

mod access;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use crate::{ManagedConnection, SQLiteError, SQLiteStorageBackend};
use uqa_core::{Edge, Value, Vertex};
use uqa_storage::PersistentStorageBackend;

use uqa_graph::{begin_graph_write, GraphStorage};
use uqa_graph::{Direction, GraphStore, GraphStoreError, GraphStoreResult, PersistentGraphStore};

const LEGACY_PROPERTIES_FORMAT: i64 = 1;
const TAGGED_PROPERTIES_FORMAT: i64 = 2;

/// Direct durable access to a standalone graph table family.
pub struct SQLiteGraphStore {
    inner: PersistentGraphStore,
    backend: Arc<dyn PersistentStorageBackend>,
    operation_gate: parking_lot::ReentrantMutex<()>,
}

fn graph_store_error(error: &GraphStoreError) -> SQLiteError {
    SQLiteError::StorageBackend(error.to_string())
}

fn sqlite_graph_error(error: &SQLiteError) -> GraphStoreError {
    GraphStoreError::Storage(error.to_string())
}

impl SQLiteGraphStore {
    /// Open a table family and migrate small access metadata once. Existing
    /// records are streamed only for a legacy label-watermark migration.
    pub fn open(conn: ManagedConnection, table_name: Option<&str>) -> Result<Self, SQLiteError> {
        let suffix = table_name.unwrap_or("");
        if !suffix
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
        {
            return Err(SQLiteError::StorageBackend(format!(
                "invalid graph table suffix {suffix:?}"
            )));
        }
        let suffix = if suffix.is_empty() {
            String::new()
        } else {
            format!("_{suffix}")
        };
        let backend: Arc<dyn PersistentStorageBackend> =
            Arc::new(SQLiteStorageBackend::new(conn.clone()));
        let storage = Arc::new(access::SQLiteGraphStorage {
            conn,
            backend: Arc::clone(&backend),
            vtx_table: format!("_graph_vertices{suffix}"),
            edge_table: format!("_graph_edges{suffix}"),
            member_table: format!("_graph_membership{suffix}"),
            catalog_table: format!("_graph_catalog{suffix}"),
            metadata_table: format!("_graph_metadata{suffix}"),
        });
        let mut inner = PersistentGraphStore::from_storage(storage.clone());
        let mut checkpoint =
            begin_graph_write(Arc::clone(&backend)).map_err(|error| graph_store_error(&error))?;
        let opened = (|| {
            storage.ensure_tables()?;
            match storage.metadata("direct_access_version")?.as_deref() {
                Some("1") => {}
                None => {
                    for graph in storage
                        .graph_names()
                        .map_err(|error| graph_store_error(&error))?
                    {
                        inner
                            .rebuild_label_registry_from_ids(&graph)
                            .map_err(|error| graph_store_error(&error))?;
                    }
                    storage.save_metadata("direct_access_version", "1")?;
                }
                Some(other) => {
                    return Err(SQLiteError::StorageBackend(format!(
                        "unsupported standalone graph access version {other}"
                    )))
                }
            }
            Ok(())
        })();
        if let Err(error) = opened {
            checkpoint
                .rollback()
                .map_err(|error| graph_store_error(&error))?;
            return Err(error);
        }
        checkpoint
            .commit()
            .map_err(|error| graph_store_error(&error))?;
        Ok(Self {
            inner,
            backend,
            operation_gate: parking_lot::ReentrantMutex::new(()),
        })
    }

    /// Execute several graph reads against one pinned physical snapshot.
    /// Callers must serialize transaction ownership on this storage session.
    pub fn read_snapshot<T>(
        &self,
        read: impl FnOnce(&PersistentGraphStore) -> GraphStoreResult<T>,
    ) -> GraphStoreResult<T> {
        struct ReadCheckpoint(Option<Arc<dyn PersistentStorageBackend>>);
        impl Drop for ReadCheckpoint {
            fn drop(&mut self) {
                if let Some(backend) = &self.0 {
                    let _ = backend.rollback_transaction();
                }
            }
        }
        let _operation = self.operation_gate.lock();
        if self.backend.in_transaction() {
            return read(&self.inner);
        }
        self.backend.begin_read_transaction()?;
        let mut checkpoint = ReadCheckpoint(Some(Arc::clone(&self.backend)));
        let result = read(&self.inner);
        self.backend.rollback_transaction()?;
        checkpoint.0 = None;
        result
    }

    /// Borrow the durable handle within an existing storage transaction.
    pub fn as_graph_store(&self) -> &PersistentGraphStore {
        &self.inner
    }

    pub fn create_graph(&mut self, name: &str) -> Result<(), SQLiteError> {
        self.inner
            .create_graph(name)
            .map_err(|error| graph_store_error(&error))
    }

    pub fn drop_graph(&mut self, name: &str) -> Result<(), SQLiteError> {
        self.inner
            .drop_graph(name)
            .map_err(|error| graph_store_error(&error))
    }

    pub fn graph_names(&self) -> Result<Vec<String>, SQLiteError> {
        self.read_snapshot(GraphStore::graph_names)
            .map_err(|error| graph_store_error(&error))
    }

    pub fn has_graph(&self, name: &str) -> Result<bool, SQLiteError> {
        self.read_snapshot(|store| store.has_graph(name))
            .map_err(|error| graph_store_error(&error))
    }

    pub fn union_graphs(&mut self, g1: &str, g2: &str, target: &str) -> Result<(), SQLiteError> {
        self.inner
            .union_graphs(g1, g2, target)
            .map_err(|error| graph_store_error(&error))
    }

    pub fn intersect_graphs(
        &mut self,
        g1: &str,
        g2: &str,
        target: &str,
    ) -> Result<(), SQLiteError> {
        self.inner
            .intersect_graphs(g1, g2, target)
            .map_err(|error| graph_store_error(&error))
    }

    pub fn difference_graphs(
        &mut self,
        g1: &str,
        g2: &str,
        target: &str,
    ) -> Result<(), SQLiteError> {
        self.inner
            .difference_graphs(g1, g2, target)
            .map_err(|error| graph_store_error(&error))
    }

    pub fn copy_graph(&mut self, source: &str, target: &str) -> Result<(), SQLiteError> {
        self.inner
            .copy_graph(source, target)
            .map_err(|error| graph_store_error(&error))
    }

    pub fn add_vertex(&mut self, vertex: Vertex, graph: &str) -> Result<(), SQLiteError> {
        self.inner
            .add_vertex(vertex, graph)
            .map_err(|error| graph_store_error(&error))
    }

    pub fn add_edge(&mut self, edge: Edge, graph: &str) -> Result<(), SQLiteError> {
        self.inner
            .add_edge(edge, graph)
            .map_err(|error| graph_store_error(&error))
    }

    pub fn remove_vertex(&mut self, vertex_id: u64, graph: &str) -> Result<(), SQLiteError> {
        self.inner
            .remove_vertex(vertex_id, graph)
            .map_err(|error| graph_store_error(&error))
    }

    pub fn remove_edge(&mut self, edge_id: u64, graph: &str) -> Result<(), SQLiteError> {
        self.inner
            .remove_edge(edge_id, graph)
            .map_err(|error| graph_store_error(&error))
    }

    pub fn neighbors(
        &self,
        vertex_id: u64,
        label: Option<&str>,
        direction: Direction,
        graph: &str,
    ) -> Result<Vec<u64>, SQLiteError> {
        self.read_snapshot(|store| store.neighbors(vertex_id, label, direction, graph))
            .map_err(|error| graph_store_error(&error))
    }

    pub fn vertices_by_label(&self, label: &str, graph: &str) -> Result<Vec<Vertex>, SQLiteError> {
        self.read_snapshot(|store| store.vertices_by_label(label, graph))
            .map_err(|error| graph_store_error(&error))
    }

    pub fn vertex_ids_by_label(&self, label: &str, graph: &str) -> Result<Vec<u64>, SQLiteError> {
        self.read_snapshot(|store| store.vertex_ids_by_label(label, graph))
            .map_err(|error| graph_store_error(&error))
    }

    pub fn vertices_in_graph(&self, graph: &str) -> Result<Vec<Vertex>, SQLiteError> {
        self.read_snapshot(|store| store.vertices_in_graph(graph))
            .map_err(|error| graph_store_error(&error))
    }

    pub fn edges_in_graph(&self, graph: &str) -> Result<Vec<Edge>, SQLiteError> {
        self.read_snapshot(|store| store.edges_in_graph(graph))
            .map_err(|error| graph_store_error(&error))
    }

    pub fn vertex_graphs(&self, vertex_id: u64) -> Result<BTreeSet<String>, SQLiteError> {
        self.read_snapshot(|store| store.vertex_graphs(vertex_id))
            .map_err(|error| graph_store_error(&error))
    }

    pub fn out_edge_ids(&self, vertex_id: u64, graph: &str) -> Result<BTreeSet<u64>, SQLiteError> {
        self.read_snapshot(|store| store.out_edge_ids(vertex_id, graph))
            .map_err(|error| graph_store_error(&error))
    }

    pub fn in_edge_ids(&self, vertex_id: u64, graph: &str) -> Result<BTreeSet<u64>, SQLiteError> {
        self.read_snapshot(|store| store.in_edge_ids(vertex_id, graph))
            .map_err(|error| graph_store_error(&error))
    }

    pub fn edge_ids_by_label(
        &self,
        label: &str,
        graph: &str,
    ) -> Result<BTreeSet<u64>, SQLiteError> {
        self.read_snapshot(|store| store.edge_ids_by_label(label, graph))
            .map_err(|error| graph_store_error(&error))
    }

    pub fn vertex_ids_in_graph(&self, graph: &str) -> Result<BTreeSet<u64>, SQLiteError> {
        self.read_snapshot(|store| store.vertex_ids_in_graph(graph))
            .map_err(|error| graph_store_error(&error))
    }

    pub fn require_vertex_in_graph(&self, vertex_id: u64, graph: &str) -> Result<(), SQLiteError> {
        self.read_snapshot(|store| store.require_vertex_in_graph(vertex_id, graph))
            .map_err(|error| graph_store_error(&error))
    }

    pub fn degree_distribution(&self, graph: &str) -> Result<BTreeMap<u64, u64>, SQLiteError> {
        self.read_snapshot(|store| store.degree_distribution(graph))
            .map_err(|error| graph_store_error(&error))
    }

    pub fn label_degree(&self, label: &str, graph: &str) -> Result<f64, SQLiteError> {
        self.read_snapshot(|store| store.label_degree(label, graph))
            .map_err(|error| graph_store_error(&error))
    }

    pub fn vertex_label_counts(&self, graph: &str) -> Result<BTreeMap<String, u64>, SQLiteError> {
        self.read_snapshot(|store| store.vertex_label_counts(graph))
            .map_err(|error| graph_store_error(&error))
    }

    pub fn get_vertex(&self, vertex_id: u64) -> Result<Option<Vertex>, SQLiteError> {
        self.read_snapshot(|store| store.get_vertex(vertex_id))
            .map_err(|error| graph_store_error(&error))
    }

    pub fn get_edge(&self, edge_id: u64) -> Result<Option<Edge>, SQLiteError> {
        self.read_snapshot(|store| store.get_edge(edge_id))
            .map_err(|error| graph_store_error(&error))
    }

    pub fn next_vertex_id(&mut self) -> Result<u64, SQLiteError> {
        self.inner
            .next_vertex_id()
            .map_err(|error| graph_store_error(&error))
    }

    pub fn next_edge_id(&mut self) -> Result<u64, SQLiteError> {
        self.inner
            .next_edge_id()
            .map_err(|error| graph_store_error(&error))
    }

    pub fn allocate_vertex_id(&mut self, label: &str, graph: &str) -> Result<u64, SQLiteError> {
        self.inner
            .allocate_vertex_id(label, graph)
            .map_err(|error| graph_store_error(&error))
    }

    pub fn allocate_edge_id(&mut self, label: &str, graph: &str) -> Result<u64, SQLiteError> {
        self.inner
            .allocate_edge_id(label, graph)
            .map_err(|error| graph_store_error(&error))
    }

    pub fn clear(&mut self) -> Result<(), SQLiteError> {
        self.inner
            .clear()
            .map_err(|error| graph_store_error(&error))
    }

    pub fn vertices(&self) -> Result<BTreeMap<u64, Vertex>, SQLiteError> {
        self.read_snapshot(GraphStore::vertices)
            .map_err(|error| graph_store_error(&error))
    }

    pub fn edges(&self) -> Result<BTreeMap<u64, Edge>, SQLiteError> {
        self.read_snapshot(GraphStore::edges)
            .map_err(|error| graph_store_error(&error))
    }
}

impl GraphStore for SQLiteGraphStore {
    fn vertex_id_page(
        &self,
        graph: &str,
        after: Option<u64>,
        limit: usize,
    ) -> GraphStoreResult<Vec<u64>> {
        self.read_snapshot(|store| store.vertex_id_page(graph, after, limit))
    }
    fn edge_id_page(
        &self,
        graph: &str,
        after: Option<u64>,
        limit: usize,
    ) -> GraphStoreResult<Vec<u64>> {
        self.read_snapshot(|store| store.edge_id_page(graph, after, limit))
    }
    fn transaction<T>(
        &mut self,
        operation: impl FnOnce(&mut Self) -> GraphStoreResult<T>,
    ) -> GraphStoreResult<T> {
        let mut checkpoint = self.inner.clone();
        checkpoint.transaction(|_| operation(self))
    }

    fn create_graph(&mut self, name: &str) -> GraphStoreResult<()> {
        self.inner.create_graph(name)
    }

    fn drop_graph(&mut self, name: &str) -> GraphStoreResult<()> {
        self.inner.drop_graph(name)
    }

    fn graph_names(&self) -> GraphStoreResult<Vec<String>> {
        self.read_snapshot(GraphStore::graph_names)
    }

    fn has_graph(&self, name: &str) -> GraphStoreResult<bool> {
        self.read_snapshot(|store| store.has_graph(name))
    }

    fn union_graphs(&mut self, g1: &str, g2: &str, target: &str) -> GraphStoreResult<()> {
        self.inner.union_graphs(g1, g2, target)
    }

    fn intersect_graphs(&mut self, g1: &str, g2: &str, target: &str) -> GraphStoreResult<()> {
        self.inner.intersect_graphs(g1, g2, target)
    }

    fn difference_graphs(&mut self, g1: &str, g2: &str, target: &str) -> GraphStoreResult<()> {
        self.inner.difference_graphs(g1, g2, target)
    }

    fn copy_graph(&mut self, source: &str, target: &str) -> GraphStoreResult<()> {
        self.inner.copy_graph(source, target)
    }

    fn add_vertex(&mut self, vertex: Vertex, graph: &str) -> GraphStoreResult<()> {
        self.inner.add_vertex(vertex, graph)
    }

    fn add_edge(&mut self, edge: Edge, graph: &str) -> GraphStoreResult<()> {
        self.inner.add_edge(edge, graph)
    }

    fn remove_vertex(&mut self, vertex_id: u64, graph: &str) -> GraphStoreResult<()> {
        self.inner.remove_vertex(vertex_id, graph)
    }

    fn remove_edge(&mut self, edge_id: u64, graph: &str) -> GraphStoreResult<()> {
        self.inner.remove_edge(edge_id, graph)
    }

    fn neighbors(
        &self,
        vertex_id: u64,
        label: Option<&str>,
        direction: Direction,
        graph: &str,
    ) -> GraphStoreResult<Vec<u64>> {
        self.read_snapshot(|store| store.neighbors(vertex_id, label, direction, graph))
    }

    fn vertices_by_label(&self, label: &str, graph: &str) -> GraphStoreResult<Vec<Vertex>> {
        self.read_snapshot(|store| store.vertices_by_label(label, graph))
    }

    fn vertex_ids_by_label(&self, label: &str, graph: &str) -> GraphStoreResult<Vec<u64>> {
        self.read_snapshot(|store| store.vertex_ids_by_label(label, graph))
    }

    fn vertices_in_graph(&self, graph: &str) -> GraphStoreResult<Vec<Vertex>> {
        self.read_snapshot(|store| store.vertices_in_graph(graph))
    }

    fn edges_in_graph(&self, graph: &str) -> GraphStoreResult<Vec<Edge>> {
        self.read_snapshot(|store| store.edges_in_graph(graph))
    }

    fn vertex_graphs(&self, vertex_id: u64) -> GraphStoreResult<BTreeSet<String>> {
        self.read_snapshot(|store| store.vertex_graphs(vertex_id))
    }
    fn edge_graphs(&self, edge_id: u64) -> GraphStoreResult<BTreeSet<String>> {
        self.read_snapshot(|store| store.edge_graphs(edge_id))
    }
    fn edges_by_label(&self, label: &str, graph: &str) -> GraphStoreResult<Vec<Edge>> {
        self.read_snapshot(|store| store.edges_by_label(label, graph))
    }

    fn out_edge_ids(&self, vertex_id: u64, graph: &str) -> GraphStoreResult<BTreeSet<u64>> {
        self.read_snapshot(|store| store.out_edge_ids(vertex_id, graph))
    }

    fn in_edge_ids(&self, vertex_id: u64, graph: &str) -> GraphStoreResult<BTreeSet<u64>> {
        self.read_snapshot(|store| store.in_edge_ids(vertex_id, graph))
    }

    fn edge_ids_by_label(&self, label: &str, graph: &str) -> GraphStoreResult<BTreeSet<u64>> {
        self.read_snapshot(|store| store.edge_ids_by_label(label, graph))
    }

    fn vertex_ids_in_graph(&self, graph: &str) -> GraphStoreResult<BTreeSet<u64>> {
        self.read_snapshot(|store| store.vertex_ids_in_graph(graph))
    }

    fn require_vertex_in_graph(&self, vertex_id: u64, graph: &str) -> GraphStoreResult<()> {
        self.read_snapshot(|store| store.require_vertex_in_graph(vertex_id, graph))
    }

    fn degree_distribution(&self, graph: &str) -> GraphStoreResult<BTreeMap<u64, u64>> {
        self.read_snapshot(|store| store.degree_distribution(graph))
    }

    fn label_degree(&self, label: &str, graph: &str) -> GraphStoreResult<f64> {
        self.read_snapshot(|store| store.label_degree(label, graph))
    }

    fn vertex_label_counts(&self, graph: &str) -> GraphStoreResult<BTreeMap<String, u64>> {
        self.read_snapshot(|store| store.vertex_label_counts(graph))
    }

    fn get_vertex(&self, vertex_id: u64) -> GraphStoreResult<Option<Vertex>> {
        self.read_snapshot(|store| store.get_vertex(vertex_id))
    }

    fn get_edge(&self, edge_id: u64) -> GraphStoreResult<Option<Edge>> {
        self.read_snapshot(|store| store.get_edge(edge_id))
    }

    fn next_vertex_id(&mut self) -> GraphStoreResult<u64> {
        self.inner.next_vertex_id()
    }

    fn next_edge_id(&mut self) -> GraphStoreResult<u64> {
        self.inner.next_edge_id()
    }

    fn allocate_vertex_id(&mut self, label: &str, graph: &str) -> GraphStoreResult<u64> {
        self.inner.allocate_vertex_id(label, graph)
    }

    fn allocate_edge_id(&mut self, label: &str, graph: &str) -> GraphStoreResult<u64> {
        self.inner.allocate_edge_id(label, graph)
    }

    fn clear(&mut self) -> GraphStoreResult<()> {
        self.inner.clear()
    }

    fn vertices(&self) -> GraphStoreResult<BTreeMap<u64, Vertex>> {
        self.read_snapshot(GraphStore::vertices)
    }

    fn edges(&self) -> GraphStoreResult<BTreeMap<u64, Edge>> {
        self.read_snapshot(GraphStore::edges)
    }
}

fn decode_graph_id(kind: &str, id: i64) -> Result<u64, SQLiteError> {
    u64::try_from(id).map_err(|_| {
        SQLiteError::StorageBackend(format!("invalid negative {kind} id {id} in graph store"))
    })
}

fn decode_properties(
    properties_json: &str,
    properties_format: i64,
) -> Result<BTreeMap<String, Value>, SQLiteError> {
    match properties_format {
        LEGACY_PROPERTIES_FORMAT => {
            let raw: BTreeMap<String, serde_json::Value> =
                serde_json::from_str(properties_json).map_err(SQLiteError::from)?;
            raw.into_iter()
                .map(|(key, value)| decode_legacy_value(value).map(|value| (key, value)))
                .collect()
        }
        TAGGED_PROPERTIES_FORMAT => {
            serde_json::from_str(properties_json).map_err(SQLiteError::from)
        }
        other => Err(SQLiteError::StorageBackend(format!(
            "unsupported graph properties format version {other}"
        ))),
    }
}

/// Decode records written by the original untagged `Value` serializer.
/// Its `Bytes(Vec<u8>)` variant preceded `List`, so a JSON array made solely
/// from byte-range integers (including `[]`) represented bytes at every
/// nesting depth. New records use an explicit bytes tag and reserve raw JSON
/// arrays for `Value::List`.
fn decode_legacy_value(raw: serde_json::Value) -> Result<Value, SQLiteError> {
    match raw {
        serde_json::Value::Array(items) => {
            let bytes = items
                .iter()
                .map(|item| item.as_u64().and_then(|number| u8::try_from(number).ok()))
                .collect::<Option<Vec<_>>>();
            if let Some(bytes) = bytes {
                return Ok(Value::Bytes(bytes));
            }
            items
                .into_iter()
                .map(decode_legacy_value)
                .collect::<Result<Vec<_>, _>>()
                .map(Value::List)
        }
        serde_json::Value::Object(map) => {
            if map
                .get("$uqa_type")
                .and_then(serde_json::Value::as_str)
                .is_some()
            {
                let tagged: Value = serde_json::from_value(serde_json::Value::Object(map.clone()))
                    .map_err(SQLiteError::from)?;
                if !matches!(tagged, Value::Map(_)) {
                    return Ok(tagged);
                }
            }
            map.into_iter()
                .map(|(key, value)| decode_legacy_value(value).map(|value| (key, value)))
                .collect::<Result<BTreeMap<_, _>, _>>()
                .map(Value::Map)
        }
        scalar => serde_json::from_value(scalar).map_err(SQLiteError::from),
    }
}

fn encode_graph_id(kind: &str, id: u64) -> Result<i64, SQLiteError> {
    i64::try_from(id).map_err(|_| {
        SQLiteError::StorageBackend(format!("{kind} id {id} exceeds SQLite INTEGER range"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use uqa_core::{Value, Vertex};

    #[test]
    fn round_trip_through_sqlite() {
        let conn = ManagedConnection::open_in_memory().unwrap();
        let mut store = SQLiteGraphStore::open(conn.clone(), None).unwrap();
        store.create_graph("g").unwrap();
        store.add_vertex(Vertex::new(1, "person"), "g").unwrap();
        store.add_vertex(Vertex::new(2, "person"), "g").unwrap();
        store.add_edge(Edge::new(1, 1, 2, "knows"), "g").unwrap();
        let other = SQLiteGraphStore::open(conn, None).unwrap();
        assert!(other.has_graph("g").unwrap());
        let vs = other.vertices_in_graph("g").unwrap();
        assert_eq!(vs.len(), 2);
        let es = other.edges_in_graph("g").unwrap();
        assert_eq!(es.len(), 1);
    }

    #[test]
    fn legacy_raw_bytes_and_edge_first_memberships_survive_reopen() {
        let conn = ManagedConnection::open_in_memory().unwrap();
        conn.with(|connection| {
            // Exact pre-versioning table shapes. Insert the edge membership
            // first to prove hydration is independent of SQLite row order.
            connection.execute_batch(
                r#"
                CREATE TABLE _graph_vertices (
                    vertex_id INTEGER PRIMARY KEY,
                    label TEXT NOT NULL DEFAULT '',
                    properties_json TEXT NOT NULL
                );
                CREATE TABLE _graph_edges (
                    edge_id INTEGER PRIMARY KEY,
                    source_id INTEGER NOT NULL,
                    target_id INTEGER NOT NULL,
                    label TEXT NOT NULL,
                    properties_json TEXT NOT NULL
                );
                CREATE TABLE _graph_membership (
                    graph TEXT NOT NULL,
                    entity_kind TEXT NOT NULL,
                    entity_id INTEGER NOT NULL,
                    PRIMARY KEY (graph, entity_kind, entity_id)
                );
                CREATE TABLE _graph_catalog (name TEXT PRIMARY KEY);
                INSERT INTO _graph_catalog (name) VALUES ('g');
                INSERT INTO _graph_vertices
                    (vertex_id, label, properties_json)
                    VALUES
                    (1, 'person', '{"bytes":[1,2],"nested":[[3,4]],"list":[256]}'),
                    (2, 'person', '{}');
                INSERT INTO _graph_edges
                    (edge_id, source_id, target_id, label, properties_json)
                    VALUES (10, 1, 2, 'knows', '{"bytes":[5,6]}');
                INSERT INTO _graph_membership
                    (graph, entity_kind, entity_id) VALUES ('g', 'e', 10);
                INSERT INTO _graph_membership
                    (graph, entity_kind, entity_id) VALUES ('g', 'v', 1);
                INSERT INTO _graph_membership
                    (graph, entity_kind, entity_id) VALUES ('g', 'v', 2);
                "#,
            )?;
            Ok(())
        })
        .unwrap();

        let reopened = SQLiteGraphStore::open(conn, None).unwrap();
        let vertex = reopened.get_vertex(1).unwrap().unwrap();
        assert_eq!(vertex.properties["bytes"], Value::Bytes(vec![1, 2]));
        assert_eq!(
            vertex.properties["nested"],
            Value::List(vec![Value::Bytes(vec![3, 4])])
        );
        assert_eq!(
            vertex.properties["list"],
            Value::List(vec![Value::Int(256)])
        );
        assert_eq!(
            reopened.get_edge(10).unwrap().unwrap().properties["bytes"],
            Value::Bytes(vec![5, 6])
        );
    }

    #[test]
    fn list_and_explicit_bytes_properties_remain_distinct_after_reopen() {
        let conn = ManagedConnection::open_in_memory().unwrap();
        let mut store = SQLiteGraphStore::open(conn.clone(), None).unwrap();
        store.create_graph("g").unwrap();
        let mut vertex = Vertex::new(1, "payload");
        vertex.properties.insert(
            "list".into(),
            Value::List(vec![Value::Int(1), Value::Int(2)]),
        );
        vertex
            .properties
            .insert("bytes".into(), Value::Bytes(vec![1, 2]));
        store.add_vertex(vertex, "g").unwrap();
        drop(store);

        let reopened = SQLiteGraphStore::open(conn, None).unwrap();
        let restored = reopened.get_vertex(1).unwrap().unwrap();
        assert_eq!(
            restored.properties["list"],
            Value::List(vec![Value::Int(1), Value::Int(2)])
        );
        assert_eq!(restored.properties["bytes"], Value::Bytes(vec![1, 2]));
    }

    #[test]
    fn failed_persistence_does_not_publish_partial_disk_state() {
        let conn = ManagedConnection::open_in_memory().unwrap();
        let mut store = SQLiteGraphStore::open(conn.clone(), None).unwrap();
        store.create_graph("g").unwrap();
        store.add_vertex(Vertex::new(1, "person"), "g").unwrap();
        conn.with(|connection| {
            connection.execute_batch(
                r#"
                CREATE TRIGGER fail_graph_membership
                BEFORE INSERT ON "_graph_membership"
                WHEN NEW.entity_id = 2
                BEGIN
                    SELECT RAISE(ABORT, 'forced graph persistence failure');
                END;
                "#,
            )?;
            Ok(())
        })
        .unwrap();

        assert!(store.add_vertex(Vertex::new(2, "person"), "g").is_err());
        assert!(store.get_vertex(1).unwrap().is_some());
        assert!(store.get_vertex(2).unwrap().is_none());

        conn.with(|connection| {
            connection.execute_batch("DROP TRIGGER fail_graph_membership")?;
            Ok(())
        })
        .unwrap();
        let reopened = SQLiteGraphStore::open(conn, None).unwrap();
        assert_eq!(reopened.vertices_in_graph("g").unwrap().len(), 1);
        assert!(reopened.get_vertex(2).unwrap().is_none());
    }

    #[test]
    fn unrelated_corrupt_payload_is_not_loaded_until_queried() {
        let conn = ManagedConnection::open_in_memory().unwrap();
        let mut store = SQLiteGraphStore::open(conn.clone(), None).unwrap();
        store.create_graph("g").unwrap();
        store.add_vertex(Vertex::new(1, "person"), "g").unwrap();
        conn.with(|connection| {
            connection.execute(
                "UPDATE _graph_vertices SET properties_json = '{' WHERE vertex_id = 1",
                [],
            )?;
            Ok(())
        })
        .unwrap();

        store.add_vertex(Vertex::new(2, "person"), "g").unwrap();
        let reopened = SQLiteGraphStore::open(conn, None).unwrap();
        assert!(reopened.get_vertex(2).unwrap().is_some());
        assert!(reopened.get_vertex(1).is_err());
    }

    #[test]
    fn allocated_label_sequence_survives_reopen() {
        let conn = ManagedConnection::open_in_memory().unwrap();
        let mut store = SQLiteGraphStore::open(conn.clone(), None).unwrap();
        store.create_graph("g").unwrap();
        let first = store.allocate_vertex_id("person", "g").unwrap();
        drop(store);

        let mut reopened = SQLiteGraphStore::open(conn, None).unwrap();
        let second = reopened.allocate_vertex_id("person", "g").unwrap();
        assert_ne!(first, second);
        assert!(second > first);
    }
}
