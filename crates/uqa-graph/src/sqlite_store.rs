//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `SQLite`-backed graph store with write-through persistence.
//!
//! A [`MemoryGraphStore`] serves
//! the in-memory query path. Each fallible mutation is first applied to a
//! candidate snapshot, persisted atomically in one `SQLite` savepoint, and
//! published to memory only after the savepoint commits. Reopened catalogs
//! therefore replay the same vertex, edge, membership, and label state.
//!
//! Tables (per optional `table_name` qualifier):
//!
//! ```sql
//! CREATE TABLE _graph_vertices_{tbl} (
//!     vertex_id        INTEGER PRIMARY KEY,
//!     label            TEXT NOT NULL DEFAULT '',
//!     properties_json  TEXT NOT NULL
//! );
//! CREATE TABLE _graph_edges_{tbl} (
//!     edge_id          INTEGER PRIMARY KEY,
//!     source_id        INTEGER NOT NULL,
//!     target_id        INTEGER NOT NULL,
//!     label            TEXT NOT NULL,
//!     properties_json  TEXT NOT NULL
//! );
//! CREATE TABLE _graph_membership_{tbl} (
//!     graph        TEXT NOT NULL,
//!     entity_kind  TEXT NOT NULL CHECK (entity_kind IN ('v', 'e')),
//!     entity_id    INTEGER NOT NULL,
//!     PRIMARY KEY (graph, entity_kind, entity_id)
//! );
//! CREATE TABLE _graph_catalog_{tbl} (
//!     name TEXT PRIMARY KEY
//! );
//! ```

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::params;
use uqa_core::{Edge, EdgeId, Value, Vertex, VertexId};
use uqa_storage::{ManagedConnection, SQLiteError};

use crate::memory_store::MemoryGraphStore;
use crate::store::{GraphStore, GraphStoreError, GraphStoreResult};
use crate::types::Direction;

const LEGACY_PROPERTIES_FORMAT: i64 = 1;
const TAGGED_PROPERTIES_FORMAT: i64 = 2;

pub struct SQLiteGraphStore {
    inner: MemoryGraphStore,
    conn: ManagedConnection,
    vtx_table: String,
    edge_table: String,
    member_table: String,
    catalog_table: String,
}

fn graph_store_error(error: &GraphStoreError) -> SQLiteError {
    SQLiteError::StorageBackend(error.to_string())
}

impl SQLiteGraphStore {
    /// Open (or create) the per-table graph tables on `conn` and
    /// rehydrate the in-memory store from any existing rows.
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
        let (vtx, edg, mem, cat) = if suffix.is_empty() {
            (
                "_graph_vertices".to_string(),
                "_graph_edges".to_string(),
                "_graph_membership".to_string(),
                "_graph_catalog".to_string(),
            )
        } else {
            (
                format!("_graph_vertices_{suffix}"),
                format!("_graph_edges_{suffix}"),
                format!("_graph_membership_{suffix}"),
                format!("_graph_catalog_{suffix}"),
            )
        };

        let mut store = Self {
            inner: MemoryGraphStore::new(),
            conn,
            vtx_table: vtx,
            edge_table: edg,
            member_table: mem,
            catalog_table: cat,
        };
        store.ensure_tables()?;
        store.load_from_sqlite()?;
        Ok(store)
    }

    fn ensure_tables(&self) -> Result<(), SQLiteError> {
        let v = &self.vtx_table;
        let e = &self.edge_table;
        let m = &self.member_table;
        let c = &self.catalog_table;
        self.conn.with(|conn| {
            conn.execute_batch(&format!(
                r#"
                CREATE TABLE IF NOT EXISTS "{v}" (
                    vertex_id INTEGER PRIMARY KEY,
                    label TEXT NOT NULL DEFAULT '',
                    properties_json TEXT NOT NULL,
                    properties_format INTEGER NOT NULL DEFAULT 2
                        CHECK (properties_format IN (1, 2))
                );
                CREATE TABLE IF NOT EXISTS "{e}" (
                    edge_id INTEGER PRIMARY KEY,
                    source_id INTEGER NOT NULL,
                    target_id INTEGER NOT NULL,
                    label TEXT NOT NULL,
                    properties_json TEXT NOT NULL,
                    properties_format INTEGER NOT NULL DEFAULT 2
                        CHECK (properties_format IN (1, 2))
                );
                CREATE TABLE IF NOT EXISTS "{m}" (
                    graph TEXT NOT NULL,
                    entity_kind TEXT NOT NULL CHECK (entity_kind IN ('v', 'e')),
                    entity_id INTEGER NOT NULL,
                    PRIMARY KEY (graph, entity_kind, entity_id)
                );
                CREATE TABLE IF NOT EXISTS "{c}" (
                    name TEXT PRIMARY KEY,
                    registry_json TEXT NOT NULL DEFAULT '{{}}'
                );
                CREATE INDEX IF NOT EXISTS "{e}_source_idx" ON "{e}" (source_id);
                CREATE INDEX IF NOT EXISTS "{e}_target_idx" ON "{e}" (target_id);
                CREATE INDEX IF NOT EXISTS "{m}_entity_idx" ON "{m}" (entity_kind, entity_id);
                "#
            ))?;
            let mut columns = conn.prepare(&format!("PRAGMA table_info(\"{c}\")"))?;
            let has_registry = columns
                .query_map([], |row| row.get::<_, String>(1))?
                .collect::<Result<Vec<_>, _>>()?
                .iter()
                .any(|column| column == "registry_json");
            if !has_registry {
                conn.execute(
                    &format!(
                        "ALTER TABLE \"{c}\" ADD COLUMN registry_json TEXT NOT NULL DEFAULT '{{}}'"
                    ),
                    [],
                )?;
            }
            for table in [&v, &e] {
                let mut columns = conn.prepare(&format!("PRAGMA table_info(\"{table}\")"))?;
                let has_properties_format = columns
                    .query_map([], |row| row.get::<_, String>(1))?
                    .collect::<Result<Vec<_>, _>>()?
                    .iter()
                    .any(|column| column == "properties_format");
                if !has_properties_format {
                    // Rows written before the tagged Value encoding used a
                    // raw JSON byte array. Mark those existing records as
                    // legacy; every engine write below explicitly stores v2.
                    conn.execute(
                        &format!(
                            "ALTER TABLE \"{table}\" ADD COLUMN properties_format \
                             INTEGER NOT NULL DEFAULT {LEGACY_PROPERTIES_FORMAT} \
                             CHECK (properties_format IN (1, 2))"
                        ),
                        [],
                    )?;
                }
            }
            Ok(())
        })
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one read transaction reconstructs a mutually consistent graph snapshot"
    )]
    fn load_from_sqlite(&mut self) -> Result<(), SQLiteError> {
        let v = self.vtx_table.clone();
        let e = self.edge_table.clone();
        let m = self.member_table.clone();
        let c = self.catalog_table.clone();
        self.conn.with(|conn| {
            // Catalog: every named graph
            let mut stmt = conn.prepare(&format!("SELECT name, registry_json FROM \"{c}\""))?;
            let graphs: Vec<(String, String)> = stmt
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<Result<_, _>>()?;
            for (name, registry_json) in &graphs {
                self.inner.create_graph(name);
                let registry = serde_json::from_str(registry_json).map_err(SQLiteError::from)?;
                self.inner.import_label_registry(name, &registry);
            }

            // Vertices
            let mut stmt = conn.prepare(&format!(
                "SELECT vertex_id, label, properties_json, properties_format FROM \"{v}\""
            ))?;
            for row in stmt.query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            })? {
                let (vid, label, props_json, properties_format) = row?;
                let properties = decode_properties(&props_json, properties_format)?;
                let vertex = Vertex {
                    vertex_id: decode_graph_id("vertex", vid)?,
                    label,
                    properties,
                };
                // Insert into the inner store. Membership is restored
                // separately so the vertex initially lives in graph 0
                // and we re-membership it below.
                self.inner
                    .insert_raw_vertex(vertex)
                    .map_err(|error| SQLiteError::StorageBackend(error.to_string()))?;
            }

            // Edges
            let mut stmt = conn.prepare(&format!(
                "SELECT edge_id, source_id, target_id, label, properties_json, \
                 properties_format FROM \"{e}\""
            ))?;
            for row in stmt.query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, i64>(5)?,
                ))
            })? {
                let (eid, src, tgt, label, props_json, properties_format) = row?;
                let properties = decode_properties(&props_json, properties_format)?;
                let edge = Edge {
                    edge_id: decode_graph_id("edge", eid)?,
                    source_id: decode_graph_id("edge source", src)?,
                    target_id: decode_graph_id("edge target", tgt)?,
                    label,
                    properties,
                };
                self.inner
                    .insert_raw_edge(edge)
                    .map_err(|error| SQLiteError::StorageBackend(error.to_string()))?;
            }

            // Membership
            let mut stmt = conn.prepare(&format!(
                "SELECT graph, entity_kind, entity_id FROM \"{m}\""
            ))?;
            let memberships = stmt
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            let mut decoded_memberships = Vec::with_capacity(memberships.len());
            for (graph, kind, id) in memberships {
                if !self.inner.has_graph(&graph) {
                    return Err(SQLiteError::StorageBackend(format!(
                        "membership references unknown graph {graph:?}"
                    )));
                }
                let id = decode_graph_id("membership", id)?;
                match kind.as_str() {
                    "v" if self.inner.get_vertex(id).is_some() => {}
                    "v" => {
                        return Err(SQLiteError::StorageBackend(format!(
                            "graph {graph:?} references missing vertex {id}"
                        )));
                    }
                    "e" if self.inner.get_edge(id).is_some() => {}
                    "e" => {
                        return Err(SQLiteError::StorageBackend(format!(
                            "graph {graph:?} references missing edge {id}"
                        )));
                    }
                    _ => {
                        return Err(SQLiteError::StorageBackend(format!(
                            "invalid graph membership kind {kind:?}"
                        )));
                    }
                }
                decoded_memberships.push((graph, kind, id));
            }
            // SQLite does not promise row order without ORDER BY. Restore
            // every vertex membership first so edge attachment can enforce
            // that both endpoints belong to the same graph partition.
            for (graph, kind, id) in &decoded_memberships {
                if kind == "v" {
                    self.inner
                        .attach_vertex(*id, graph)
                        .map_err(|error| SQLiteError::StorageBackend(error.to_string()))?;
                }
            }
            for (graph, kind, id) in &decoded_memberships {
                if kind == "e" {
                    self.inner
                        .attach_edge(*id, graph)
                        .map_err(|error| SQLiteError::StorageBackend(error.to_string()))?;
                }
            }
            for (name, _) in &graphs {
                self.inner.rebuild_label_registry_from_ids(name);
            }
            Ok(())
        })
    }

    /// Persist a complete, internally consistent graph snapshot in one
    /// savepoint. The in-memory candidate is published only after this
    /// transaction commits, so a storage failure cannot create a state that
    /// appears successful until the next reopen.
    #[expect(
        clippy::too_many_lines,
        reason = "one savepoint writes every graph registry before memory publication"
    )]
    fn persist_snapshot(&self, snapshot: &MemoryGraphStore) -> Result<(), SQLiteError> {
        let graph_rows: Vec<(String, String)> = snapshot
            .graph_names()
            .into_iter()
            .map(|name| {
                let registry = serde_json::to_string(&snapshot.label_registry(&name))?;
                Ok((name, registry))
            })
            .collect::<Result<_, SQLiteError>>()?;
        let vertex_rows: Vec<(i64, String, String)> = snapshot
            .vertices()
            .into_values()
            .map(|vertex| {
                Ok((
                    encode_graph_id("vertex", vertex.vertex_id)?,
                    vertex.label,
                    serde_json::to_string(&vertex.properties)?,
                ))
            })
            .collect::<Result<_, SQLiteError>>()?;
        let edge_rows: Vec<(i64, i64, i64, String, String)> = snapshot
            .edges()
            .into_values()
            .map(|edge| {
                Ok((
                    encode_graph_id("edge", edge.edge_id)?,
                    encode_graph_id("edge source", edge.source_id)?,
                    encode_graph_id("edge target", edge.target_id)?,
                    edge.label,
                    serde_json::to_string(&edge.properties)?,
                ))
            })
            .collect::<Result<_, SQLiteError>>()?;
        let mut memberships = Vec::new();
        for (graph, _) in &graph_rows {
            for vertex_id in snapshot
                .vertex_ids_in_graph(graph)
                .map_err(|error| graph_store_error(&error))?
            {
                memberships.push((
                    graph.clone(),
                    "v",
                    encode_graph_id("vertex membership", vertex_id)?,
                ));
            }
            for edge_id in snapshot
                .out_edge_ids_for_graph(graph)
                .map_err(|error| SQLiteError::StorageBackend(error.to_string()))?
            {
                memberships.push((
                    graph.clone(),
                    "e",
                    encode_graph_id("edge membership", edge_id)?,
                ));
            }
        }

        let vertex_table = self.vtx_table.clone();
        let edge_table = self.edge_table.clone();
        let member_table = self.member_table.clone();
        let catalog_table = self.catalog_table.clone();
        self.conn.with_mut(|connection| {
            let savepoint = connection.savepoint()?;
            savepoint.execute(&format!("DELETE FROM \"{member_table}\""), [])?;
            savepoint.execute(&format!("DELETE FROM \"{catalog_table}\""), [])?;
            savepoint.execute(&format!("DELETE FROM \"{edge_table}\""), [])?;
            savepoint.execute(&format!("DELETE FROM \"{vertex_table}\""), [])?;
            for (name, registry_json) in &graph_rows {
                savepoint.execute(
                    &format!(
                        "INSERT INTO \"{catalog_table}\" (name, registry_json) VALUES (?1, ?2)"
                    ),
                    params![name, registry_json],
                )?;
            }
            for (vertex_id, label, properties_json) in &vertex_rows {
                savepoint.execute(
                    &format!(
                        "INSERT INTO \"{vertex_table}\" \
                         (vertex_id, label, properties_json, properties_format) \
                         VALUES (?1, ?2, ?3, ?4)"
                    ),
                    params![
                        vertex_id,
                        label,
                        properties_json,
                        TAGGED_PROPERTIES_FORMAT
                    ],
                )?;
            }
            for (edge_id, source_id, target_id, label, properties_json) in &edge_rows {
                savepoint.execute(
                    &format!(
                        "INSERT INTO \"{edge_table}\" \
                         (edge_id, source_id, target_id, label, properties_json, properties_format) \
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6)"
                    ),
                    params![
                        edge_id,
                        source_id,
                        target_id,
                        label,
                        properties_json,
                        TAGGED_PROPERTIES_FORMAT
                    ],
                )?;
            }
            for (graph, kind, entity_id) in &memberships {
                savepoint.execute(
                    &format!(
                        "INSERT INTO \"{member_table}\" \
                         (graph, entity_kind, entity_id) VALUES (?1, ?2, ?3)"
                    ),
                    params![graph, kind, entity_id],
                )?;
            }
            savepoint.commit()?;
            Ok(())
        })
    }

    fn apply_mutation(
        &mut self,
        mutation: impl FnOnce(&mut MemoryGraphStore) -> GraphStoreResult<()>,
    ) -> Result<(), SQLiteError> {
        let mut candidate = self.inner.clone();
        mutation(&mut candidate).map_err(|error| graph_store_error(&error))?;
        self.persist_snapshot(&candidate)?;
        self.inner = candidate;
        Ok(())
    }

    fn require_graph(&self, graph: &str) -> Result<(), SQLiteError> {
        if self.inner.has_graph(graph) {
            Ok(())
        } else {
            Err(SQLiteError::StorageBackend(format!(
                "unknown graph {graph:?}"
            )))
        }
    }

    pub fn as_memory_store(&self) -> &MemoryGraphStore {
        &self.inner
    }

    pub fn create_graph(&mut self, name: &str) -> Result<(), SQLiteError> {
        self.apply_mutation(|store| {
            store.create_graph(name);
            Ok(())
        })
    }

    pub fn drop_graph(&mut self, name: &str) -> Result<(), SQLiteError> {
        self.apply_mutation(|store| {
            store.drop_graph(name);
            Ok(())
        })
    }

    pub fn union_graphs(
        &mut self,
        left: &str,
        right: &str,
        target: &str,
    ) -> Result<(), SQLiteError> {
        self.require_graph(left)?;
        self.require_graph(right)?;
        self.apply_mutation(|store| store.union_graphs(left, right, target))
    }

    pub fn intersect_graphs(
        &mut self,
        left: &str,
        right: &str,
        target: &str,
    ) -> Result<(), SQLiteError> {
        self.require_graph(left)?;
        self.require_graph(right)?;
        self.apply_mutation(|store| store.intersect_graphs(left, right, target))
    }

    pub fn difference_graphs(
        &mut self,
        left: &str,
        right: &str,
        target: &str,
    ) -> Result<(), SQLiteError> {
        self.require_graph(left)?;
        self.require_graph(right)?;
        self.apply_mutation(|store| store.difference_graphs(left, right, target))
    }

    pub fn copy_graph(&mut self, source: &str, target: &str) -> Result<(), SQLiteError> {
        self.require_graph(source)?;
        self.apply_mutation(|store| store.copy_graph(source, target))
    }

    pub fn add_vertex(&mut self, vertex: Vertex, graph: &str) -> Result<(), SQLiteError> {
        self.apply_mutation(|store| store.add_vertex(vertex, graph))
    }

    pub fn add_edge(&mut self, edge: Edge, graph: &str) -> Result<(), SQLiteError> {
        self.apply_mutation(|store| store.add_edge(edge, graph))
    }

    pub fn remove_vertex(&mut self, vertex_id: VertexId, graph: &str) -> Result<(), SQLiteError> {
        self.apply_mutation(|store| store.remove_vertex(vertex_id, graph))
    }

    pub fn remove_edge(&mut self, edge_id: EdgeId, graph: &str) -> Result<(), SQLiteError> {
        self.apply_mutation(|store| store.remove_edge(edge_id, graph))
    }

    pub fn allocate_vertex_id(
        &mut self,
        label: &str,
        graph: &str,
    ) -> Result<VertexId, SQLiteError> {
        self.require_graph(graph)?;
        let mut candidate = self.inner.clone();
        let id = candidate
            .allocate_vertex_id(label, graph)
            .map_err(|error| SQLiteError::StorageBackend(error.to_string()))?;
        self.persist_snapshot(&candidate)?;
        self.inner = candidate;
        Ok(id)
    }

    pub fn allocate_edge_id(&mut self, label: &str, graph: &str) -> Result<EdgeId, SQLiteError> {
        self.require_graph(graph)?;
        let mut candidate = self.inner.clone();
        let id = candidate
            .allocate_edge_id(label, graph)
            .map_err(|error| SQLiteError::StorageBackend(error.to_string()))?;
        self.persist_snapshot(&candidate)?;
        self.inner = candidate;
        Ok(id)
    }

    pub fn clear(&mut self) -> Result<(), SQLiteError> {
        self.apply_mutation(|store| {
            store.clear();
            Ok(())
        })
    }

    pub fn graph_names(&self) -> Vec<String> {
        self.inner.graph_names()
    }

    pub fn has_graph(&self, name: &str) -> bool {
        self.inner.has_graph(name)
    }

    pub fn neighbors(
        &self,
        vertex_id: VertexId,
        label: Option<&str>,
        direction: Direction,
        graph: &str,
    ) -> Result<Vec<VertexId>, SQLiteError> {
        self.require_graph(graph)?;
        self.inner
            .neighbors(vertex_id, label, direction, graph)
            .map_err(|error| graph_store_error(&error))
    }

    pub fn vertices_by_label(&self, label: &str, graph: &str) -> Result<Vec<Vertex>, SQLiteError> {
        self.require_graph(graph)?;
        self.inner
            .vertices_by_label(label, graph)
            .map_err(|error| graph_store_error(&error))
    }

    pub fn vertex_ids_by_label(
        &self,
        label: &str,
        graph: &str,
    ) -> Result<Vec<VertexId>, SQLiteError> {
        self.require_graph(graph)?;
        self.inner
            .vertex_ids_by_label(label, graph)
            .map_err(|error| graph_store_error(&error))
    }

    pub fn vertices_in_graph(&self, graph: &str) -> Result<Vec<Vertex>, SQLiteError> {
        self.require_graph(graph)?;
        self.inner
            .vertices_in_graph(graph)
            .map_err(|error| graph_store_error(&error))
    }

    pub fn edges_in_graph(&self, graph: &str) -> Result<Vec<Edge>, SQLiteError> {
        self.require_graph(graph)?;
        self.inner
            .edges_in_graph(graph)
            .map_err(|error| graph_store_error(&error))
    }

    pub fn vertex_graphs(&self, vertex_id: VertexId) -> BTreeSet<String> {
        self.inner.vertex_graphs(vertex_id)
    }

    pub fn get_vertex(&self, vertex_id: VertexId) -> Option<&Vertex> {
        self.inner.get_vertex(vertex_id)
    }

    pub fn get_edge(&self, edge_id: EdgeId) -> Option<&Edge> {
        self.inner.get_edge(edge_id)
    }

    pub fn vertices(&self) -> BTreeMap<VertexId, Vertex> {
        self.inner.vertices()
    }

    pub fn edges(&self) -> BTreeMap<EdgeId, Edge> {
        self.inner.edges()
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
        assert!(other.has_graph("g"));
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
        let vertex = reopened.get_vertex(1).unwrap();
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
            reopened.get_edge(10).unwrap().properties["bytes"],
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
        let restored = reopened.get_vertex(1).unwrap();
        assert_eq!(
            restored.properties["list"],
            Value::List(vec![Value::Int(1), Value::Int(2)])
        );
        assert_eq!(restored.properties["bytes"], Value::Bytes(vec![1, 2]));
    }

    #[test]
    fn failed_persistence_does_not_publish_memory_or_partial_disk_state() {
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
        assert!(store.get_vertex(1).is_some());
        assert!(store.get_vertex(2).is_none());

        conn.with(|connection| {
            connection.execute_batch("DROP TRIGGER fail_graph_membership")?;
            Ok(())
        })
        .unwrap();
        let reopened = SQLiteGraphStore::open(conn, None).unwrap();
        assert_eq!(reopened.vertices_in_graph("g").unwrap().len(), 1);
        assert!(reopened.get_vertex(2).is_none());
    }

    #[test]
    fn corrupt_property_json_is_an_open_error() {
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

        assert!(SQLiteGraphStore::open(conn, None).is_err());
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
