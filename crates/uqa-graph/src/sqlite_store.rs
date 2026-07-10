//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `SQLite`-backed graph store with write-through persistence.
//!
//! Mirrors UQA `storage/sqlite_graph_store`. Wraps a
//! [`MemoryGraphStore`] for the in-memory query path and routes every
//! mutation through the [`crate::store::GraphStore`] trait into a
//! `SQLite` connection so reopened catalogs replay the same vertex /
//! edge / membership state.
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

#![allow(clippy::too_many_lines)]

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::params;
use uqa_core::{Edge, EdgeId, Value, Vertex, VertexId};
use uqa_storage::{ManagedConnection, SQLiteError};

use crate::memory_store::MemoryGraphStore;
use crate::store::GraphStore;
use crate::types::Direction;

pub struct SQLiteGraphStore {
    inner: MemoryGraphStore,
    conn: ManagedConnection,
    vtx_table: String,
    edge_table: String,
    member_table: String,
    catalog_table: String,
}

impl SQLiteGraphStore {
    /// Open (or create) the per-table graph tables on `conn` and
    /// rehydrate the in-memory store from any existing rows.
    pub fn open(conn: ManagedConnection, table_name: Option<&str>) -> Result<Self, SQLiteError> {
        let suffix = table_name.unwrap_or("");
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
                    properties_json TEXT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS "{e}" (
                    edge_id INTEGER PRIMARY KEY,
                    source_id INTEGER NOT NULL,
                    target_id INTEGER NOT NULL,
                    label TEXT NOT NULL,
                    properties_json TEXT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS "{m}" (
                    graph TEXT NOT NULL,
                    entity_kind TEXT NOT NULL CHECK (entity_kind IN ('v', 'e')),
                    entity_id INTEGER NOT NULL,
                    PRIMARY KEY (graph, entity_kind, entity_id)
                );
                CREATE TABLE IF NOT EXISTS "{c}" (
                    name TEXT PRIMARY KEY
                );
                CREATE INDEX IF NOT EXISTS "{e}_source_idx" ON "{e}" (source_id);
                CREATE INDEX IF NOT EXISTS "{e}_target_idx" ON "{e}" (target_id);
                CREATE INDEX IF NOT EXISTS "{m}_entity_idx" ON "{m}" (entity_kind, entity_id);
                "#
            ))?;
            Ok(())
        })
    }

    fn load_from_sqlite(&mut self) -> Result<(), SQLiteError> {
        let v = self.vtx_table.clone();
        let e = self.edge_table.clone();
        let m = self.member_table.clone();
        let c = self.catalog_table.clone();
        self.conn.with(|conn| {
            // Catalog: every named graph
            let mut stmt = conn.prepare(&format!("SELECT name FROM \"{c}\""))?;
            let names: Vec<String> = stmt
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<Result<_, _>>()?;
            for name in &names {
                self.inner.create_graph(name);
            }

            // Vertices
            let mut stmt = conn.prepare(&format!(
                "SELECT vertex_id, label, properties_json FROM \"{v}\""
            ))?;
            for row in stmt.query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })? {
                let (vid, label, props_json) = row?;
                let properties: BTreeMap<String, Value> =
                    serde_json::from_str(&props_json).unwrap_or_default();
                let vertex = Vertex {
                    vertex_id: vid as VertexId,
                    label,
                    properties,
                };
                // Insert into the inner store. Membership is restored
                // separately so the vertex initially lives in graph 0
                // and we re-membership it below.
                self.inner.insert_raw_vertex(vertex);
            }

            // Edges
            let mut stmt = conn.prepare(&format!(
                "SELECT edge_id, source_id, target_id, label, properties_json FROM \"{e}\""
            ))?;
            for row in stmt.query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                ))
            })? {
                let (eid, src, tgt, label, props_json) = row?;
                let properties: BTreeMap<String, Value> =
                    serde_json::from_str(&props_json).unwrap_or_default();
                let edge = Edge {
                    edge_id: eid as EdgeId,
                    source_id: src as VertexId,
                    target_id: tgt as VertexId,
                    label,
                    properties,
                };
                self.inner.insert_raw_edge(edge);
            }

            // Membership
            let mut stmt = conn.prepare(&format!(
                "SELECT graph, entity_kind, entity_id FROM \"{m}\""
            ))?;
            for row in stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })? {
                let (graph, kind, id) = row?;
                self.inner.create_graph(&graph);
                if kind == "v" {
                    self.inner.attach_vertex(id as VertexId, &graph);
                } else if kind == "e" {
                    self.inner.attach_edge(id as EdgeId, &graph);
                }
            }
            Ok(())
        })
    }

    fn write_vertex(&self, v: &Vertex) -> Result<(), SQLiteError> {
        let props = serde_json::to_string(&v.properties).map_err(SQLiteError::from)?;
        let table = self.vtx_table.clone();
        self.conn.with(|c| {
            c.execute(
                &format!(
                    "INSERT OR REPLACE INTO \"{table}\" \
                     (vertex_id, label, properties_json) VALUES (?1, ?2, ?3)"
                ),
                params![v.vertex_id as i64, v.label, props],
            )?;
            Ok(())
        })
    }

    fn write_edge(&self, e: &Edge) -> Result<(), SQLiteError> {
        let props = serde_json::to_string(&e.properties).map_err(SQLiteError::from)?;
        let table = self.edge_table.clone();
        self.conn.with(|c| {
            c.execute(
                &format!(
                    "INSERT OR REPLACE INTO \"{table}\" \
                     (edge_id, source_id, target_id, label, properties_json) \
                     VALUES (?1, ?2, ?3, ?4, ?5)"
                ),
                params![
                    e.edge_id as i64,
                    e.source_id as i64,
                    e.target_id as i64,
                    e.label,
                    props
                ],
            )?;
            Ok(())
        })
    }

    fn write_membership(&self, graph: &str, kind: &str, id: i64) -> Result<(), SQLiteError> {
        let table = self.member_table.clone();
        let g = graph.to_string();
        let k = kind.to_string();
        self.conn.with(|c| {
            c.execute(
                &format!(
                    "INSERT OR IGNORE INTO \"{table}\" \
                     (graph, entity_kind, entity_id) VALUES (?1, ?2, ?3)"
                ),
                params![g, k, id],
            )?;
            Ok(())
        })
    }

    fn delete_membership(&self, graph: &str, kind: &str, id: i64) -> Result<(), SQLiteError> {
        let table = self.member_table.clone();
        let g = graph.to_string();
        let k = kind.to_string();
        self.conn.with(|c| {
            c.execute(
                &format!(
                    "DELETE FROM \"{table}\" \
                     WHERE graph = ?1 AND entity_kind = ?2 AND entity_id = ?3"
                ),
                params![g, k, id],
            )?;
            Ok(())
        })
    }

    fn write_catalog(&self, name: &str) -> Result<(), SQLiteError> {
        let table = self.catalog_table.clone();
        let n = name.to_string();
        self.conn.with(|c| {
            c.execute(
                &format!("INSERT OR IGNORE INTO \"{table}\" (name) VALUES (?1)"),
                params![n],
            )?;
            Ok(())
        })
    }

    fn delete_catalog(&self, name: &str) -> Result<(), SQLiteError> {
        let table = self.catalog_table.clone();
        let n = name.to_string();
        self.conn.with(|c| {
            c.execute(
                &format!("DELETE FROM \"{table}\" WHERE name = ?1"),
                params![n],
            )?;
            Ok(())
        })
    }

    fn delete_vertex_row(&self, id: i64) -> Result<(), SQLiteError> {
        let table = self.vtx_table.clone();
        self.conn.with(|c| {
            c.execute(
                &format!("DELETE FROM \"{table}\" WHERE vertex_id = ?1"),
                params![id],
            )?;
            Ok(())
        })
    }

    fn delete_edge_row(&self, id: i64) -> Result<(), SQLiteError> {
        let table = self.edge_table.clone();
        self.conn.with(|c| {
            c.execute(
                &format!("DELETE FROM \"{table}\" WHERE edge_id = ?1"),
                params![id],
            )?;
            Ok(())
        })
    }
}

impl GraphStore for SQLiteGraphStore {
    fn create_graph(&mut self, name: &str) {
        self.inner.create_graph(name);
        let _ = self.write_catalog(name);
    }

    fn drop_graph(&mut self, name: &str) {
        // Snapshot vertex/edge ids in this graph before the inner
        // store releases them so we can drop their SQLite rows.
        let v_ids = self.inner.vertex_ids_in_graph(name);
        let e_ids = self.inner.out_edge_ids_for_graph(name);
        self.inner.drop_graph(name);

        let _ = self.delete_catalog(name);
        for vid in &v_ids {
            let _ = self.delete_membership(name, "v", *vid as i64);
        }
        for eid in &e_ids {
            let _ = self.delete_membership(name, "e", *eid as i64);
        }
        // Rows that no longer belong to any graph are released by the
        // inner store; sweep their rows from SQLite too.
        for vid in &v_ids {
            if self.inner.get_vertex(*vid).is_none() {
                let _ = self.delete_vertex_row(*vid as i64);
            }
        }
        for eid in &e_ids {
            if self.inner.get_edge(*eid).is_none() {
                let _ = self.delete_edge_row(*eid as i64);
            }
        }
    }

    fn graph_names(&self) -> Vec<String> {
        self.inner.graph_names()
    }

    fn has_graph(&self, name: &str) -> bool {
        self.inner.has_graph(name)
    }

    fn union_graphs(&mut self, g1: &str, g2: &str, target: &str) {
        self.inner.union_graphs(g1, g2, target);
        let _ = self.write_catalog(target);
        for vid in self.inner.vertex_ids_in_graph(target) {
            let _ = self.write_membership(target, "v", vid as i64);
        }
        for eid in self.inner.out_edge_ids_for_graph(target) {
            let _ = self.write_membership(target, "e", eid as i64);
        }
    }

    fn intersect_graphs(&mut self, g1: &str, g2: &str, target: &str) {
        self.inner.intersect_graphs(g1, g2, target);
        let _ = self.write_catalog(target);
        for vid in self.inner.vertex_ids_in_graph(target) {
            let _ = self.write_membership(target, "v", vid as i64);
        }
        for eid in self.inner.out_edge_ids_for_graph(target) {
            let _ = self.write_membership(target, "e", eid as i64);
        }
    }

    fn difference_graphs(&mut self, g1: &str, g2: &str, target: &str) {
        self.inner.difference_graphs(g1, g2, target);
        let _ = self.write_catalog(target);
        for vid in self.inner.vertex_ids_in_graph(target) {
            let _ = self.write_membership(target, "v", vid as i64);
        }
        for eid in self.inner.out_edge_ids_for_graph(target) {
            let _ = self.write_membership(target, "e", eid as i64);
        }
    }

    fn copy_graph(&mut self, source: &str, target: &str) {
        self.inner.copy_graph(source, target);
        let _ = self.write_catalog(target);
        for vid in self.inner.vertex_ids_in_graph(target) {
            let _ = self.write_membership(target, "v", vid as i64);
        }
        for eid in self.inner.out_edge_ids_for_graph(target) {
            let _ = self.write_membership(target, "e", eid as i64);
        }
    }

    fn add_vertex(&mut self, vertex: Vertex, graph: &str) {
        let vid = vertex.vertex_id;
        let v_clone = vertex.clone();
        self.inner.add_vertex(vertex, graph);
        let _ = self.write_vertex(&v_clone);
        let _ = self.write_catalog(graph);
        let _ = self.write_membership(graph, "v", vid as i64);
    }

    fn add_edge(&mut self, edge: Edge, graph: &str) {
        let eid = edge.edge_id;
        let e_clone = edge.clone();
        self.inner.add_edge(edge, graph);
        let _ = self.write_edge(&e_clone);
        let _ = self.write_catalog(graph);
        let _ = self.write_membership(graph, "e", eid as i64);
    }

    fn remove_vertex(&mut self, vertex_id: VertexId, graph: &str) {
        self.inner.remove_vertex(vertex_id, graph);
        let _ = self.delete_membership(graph, "v", vertex_id as i64);
        if self.inner.get_vertex(vertex_id).is_none() {
            let _ = self.delete_vertex_row(vertex_id as i64);
        }
    }

    fn remove_edge(&mut self, edge_id: EdgeId, graph: &str) {
        self.inner.remove_edge(edge_id, graph);
        let _ = self.delete_membership(graph, "e", edge_id as i64);
        if self.inner.get_edge(edge_id).is_none() {
            let _ = self.delete_edge_row(edge_id as i64);
        }
    }

    fn neighbors(
        &self,
        vertex_id: VertexId,
        label: Option<&str>,
        direction: Direction,
        graph: &str,
    ) -> Vec<VertexId> {
        self.inner.neighbors(vertex_id, label, direction, graph)
    }

    fn vertices_by_label(&self, label: &str, graph: &str) -> Vec<Vertex> {
        self.inner.vertices_by_label(label, graph)
    }

    fn vertices_in_graph(&self, graph: &str) -> Vec<Vertex> {
        self.inner.vertices_in_graph(graph)
    }

    fn edges_in_graph(&self, graph: &str) -> Vec<Edge> {
        self.inner.edges_in_graph(graph)
    }

    fn vertex_graphs(&self, vertex_id: VertexId) -> BTreeSet<String> {
        self.inner.vertex_graphs(vertex_id)
    }

    fn out_edge_ids(&self, vertex_id: VertexId, graph: &str) -> BTreeSet<EdgeId> {
        self.inner.out_edge_ids(vertex_id, graph)
    }

    fn in_edge_ids(&self, vertex_id: VertexId, graph: &str) -> BTreeSet<EdgeId> {
        self.inner.in_edge_ids(vertex_id, graph)
    }

    fn edge_ids_by_label(&self, label: &str, graph: &str) -> BTreeSet<EdgeId> {
        self.inner.edge_ids_by_label(label, graph)
    }

    fn vertex_ids_in_graph(&self, graph: &str) -> BTreeSet<VertexId> {
        self.inner.vertex_ids_in_graph(graph)
    }

    fn degree_distribution(&self, graph: &str) -> BTreeMap<VertexId, u64> {
        self.inner.degree_distribution(graph)
    }

    fn label_degree(&self, label: &str, graph: &str) -> f64 {
        self.inner.label_degree(label, graph)
    }

    fn vertex_label_counts(&self, graph: &str) -> BTreeMap<String, u64> {
        self.inner.vertex_label_counts(graph)
    }

    fn get_vertex(&self, vertex_id: VertexId) -> Option<&Vertex> {
        self.inner.get_vertex(vertex_id)
    }

    fn get_edge(&self, edge_id: EdgeId) -> Option<&Edge> {
        self.inner.get_edge(edge_id)
    }

    fn next_vertex_id(&mut self) -> VertexId {
        self.inner.next_vertex_id()
    }

    fn next_edge_id(&mut self) -> EdgeId {
        self.inner.next_edge_id()
    }

    fn allocate_vertex_id(&mut self, label: &str, graph: &str) -> VertexId {
        self.inner.allocate_vertex_id(label, graph)
    }

    fn allocate_edge_id(&mut self, label: &str, graph: &str) -> EdgeId {
        self.inner.allocate_edge_id(label, graph)
    }

    fn clear(&mut self) {
        let v_table = self.vtx_table.clone();
        let e_table = self.edge_table.clone();
        let m_table = self.member_table.clone();
        let c_table = self.catalog_table.clone();
        let _ = self.conn.with(|conn| {
            conn.execute(&format!("DELETE FROM \"{v_table}\""), [])?;
            conn.execute(&format!("DELETE FROM \"{e_table}\""), [])?;
            conn.execute(&format!("DELETE FROM \"{m_table}\""), [])?;
            conn.execute(&format!("DELETE FROM \"{c_table}\""), [])?;
            Ok(())
        });
        self.inner.clear();
    }

    fn vertices(&self) -> BTreeMap<VertexId, Vertex> {
        self.inner.vertices()
    }

    fn edges(&self) -> BTreeMap<EdgeId, Edge> {
        self.inner.edges()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uqa_core::Vertex;

    #[test]
    fn round_trip_through_sqlite() {
        let conn = ManagedConnection::open_in_memory().unwrap();
        let mut store = SQLiteGraphStore::open(conn.clone(), None).unwrap();
        store.create_graph("g");
        store.add_vertex(Vertex::new(1, "person"), "g");
        store.add_vertex(Vertex::new(2, "person"), "g");
        store.add_edge(Edge::new(1, 1, 2, "knows"), "g");
        let other = SQLiteGraphStore::open(conn, None).unwrap();
        assert!(other.has_graph("g"));
        let vs = other.vertices_in_graph("g");
        assert_eq!(vs.len(), 2);
        let es = other.edges_in_graph("g");
        assert_eq!(es.len(), 1);
    }
}
