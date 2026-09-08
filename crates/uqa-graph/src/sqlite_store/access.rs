//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical access for the standalone, optionally qualified `SQLite` graph tables.

use std::fmt::Write as _;

use super::{
    decode_graph_id, decode_properties, encode_graph_id, sqlite_graph_error,
    LEGACY_PROPERTIES_FORMAT, TAGGED_PROPERTIES_FORMAT,
};
use crate::persistent_store::storage::{begin_graph_write, GraphStorage, GraphWriteTransaction};
use crate::{GraphLabelRegistry, GraphStoreError, GraphStoreResult};
use rusqlite::{params, params_from_iter, types::Value as SQLValue, OptionalExtension};
use std::sync::Arc;
use uqa_core::{Edge, Vertex};
use uqa_storage::{
    GraphEntityFilter, GraphEntityKind, ManagedConnection, PersistentStorageBackend, SQLiteError,
    MAX_GRAPH_ID_PAGE,
};

pub(super) struct SQLiteGraphStorage {
    pub conn: ManagedConnection,
    pub backend: Arc<dyn PersistentStorageBackend>,
    pub vtx_table: String,
    pub edge_table: String,
    pub member_table: String,
    pub catalog_table: String,
    pub metadata_table: String,
}

fn kind_key(kind: GraphEntityKind) -> &'static str {
    match kind {
        GraphEntityKind::Vertex => "v",
        GraphEntityKind::Edge => "e",
    }
}

struct Selection {
    from: String,
    id: String,
    stored_id: String,
    values: Vec<SQLValue>,
}

impl SQLiteGraphStorage {
    fn sql<T>(
        &self,
        read: impl FnOnce(&rusqlite::Connection) -> Result<T, SQLiteError>,
    ) -> GraphStoreResult<T> {
        self.conn
            .with(read)
            .map_err(|error| sqlite_graph_error(&error))
    }

    pub(super) fn metadata(&self, key: &str) -> Result<Option<String>, SQLiteError> {
        self.conn.with(|conn| {
            Ok(conn
                .query_row(
                    &format!(
                        "SELECT value FROM \"{}\" WHERE key = ?1",
                        self.metadata_table
                    ),
                    [key],
                    |row| row.get(0),
                )
                .optional()?)
        })
    }
    pub(super) fn save_metadata(&self, key: &str, value: &str) -> Result<(), SQLiteError> {
        self.conn.with(|conn| {
            conn.execute(
                &format!("INSERT INTO \"{}\" (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value", self.metadata_table),
                params![key, value],
            )?;
            Ok(())
        })
    }
    pub(super) fn ensure_tables(&self) -> Result<(), SQLiteError> {
        let v = &self.vtx_table;
        let e = &self.edge_table;
        let m = &self.member_table;
        let c = &self.catalog_table;
        let metadata = &self.metadata_table;
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
                CREATE TABLE IF NOT EXISTS "{metadata}" (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                CREATE INDEX IF NOT EXISTS "{v}_label_idx" ON "{v}" (label, vertex_id);
                CREATE INDEX IF NOT EXISTS "{e}_label_idx" ON "{e}" (label, edge_id);
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

    fn entity_table(&self, kind: GraphEntityKind) -> (&str, &str) {
        match kind {
            GraphEntityKind::Vertex => (&self.vtx_table, "vertex_id"),
            GraphEntityKind::Edge => (&self.edge_table, "edge_id"),
        }
    }

    fn selection(&self, filter: GraphEntityFilter<'_>) -> GraphStoreResult<Selection> {
        filter.validate()?;
        let (table, key) = self.entity_table(filter.kind);
        let membership = &self.member_table;
        let mut values = Vec::new();
        let index = match (filter.kind, filter.source, filter.target, filter.label) {
            (GraphEntityKind::Edge, Some(_), _, _) => Some(format!("{table}_source_idx")),
            (GraphEntityKind::Edge, _, Some(_), _) => Some(format!("{table}_target_idx")),
            (_, _, _, Some(_)) => Some(format!("{table}_label_idx")),
            _ => None,
        };
        let (mut from, id) = if let Some(index) = &index {
            (
                format!("FROM \"{table}\" AS e INDEXED BY \"{index}\" WHERE 1 = 1"),
                format!("e.{key}"),
            )
        } else if let Some(graph) = filter.graph {
            values.push(SQLValue::Text(graph.to_owned()));
            values.push(SQLValue::Text(kind_key(filter.kind).to_owned()));
            (format!("FROM \"{membership}\" AS m LEFT JOIN \"{table}\" AS e ON e.{key} = m.entity_id WHERE m.graph = ? AND m.entity_kind = ?"), "m.entity_id".to_owned())
        } else {
            (
                format!("FROM \"{table}\" AS e WHERE 1 = 1"),
                format!("e.{key}"),
            )
        };
        if let Some(label) = filter.label {
            from.push_str(" AND e.label = ?");
            values.push(SQLValue::Text(label.to_owned()));
        }
        for (column, id) in [("source_id", filter.source), ("target_id", filter.target)] {
            if let Some(id) = id {
                write!(from, " AND e.{column} = ?").expect("write endpoint predicate");
                values.push(SQLValue::Integer(
                    encode_graph_id("endpoint", id).map_err(|error| sqlite_graph_error(&error))?,
                ));
            }
        }
        if index.is_some() {
            if let Some(graph) = filter.graph {
                write!(from, " AND EXISTS (SELECT 1 FROM \"{membership}\" AS m WHERE m.entity_kind = ? AND m.entity_id = e.{key} AND m.graph = ?)").expect("write membership predicate");
                values.push(SQLValue::Text(kind_key(filter.kind).to_owned()));
                values.push(SQLValue::Text(graph.to_owned()));
            }
        }
        Ok(Selection {
            from,
            id,
            stored_id: format!("e.{key}"),
            values,
        })
    }

    fn delete_entity(&self, kind: GraphEntityKind, id: u64) -> GraphStoreResult<()> {
        let (table, key) = self.entity_table(kind);
        self.sql(|conn| {
            let id = encode_graph_id("entity", id)?;
            conn.execute(
                &format!(
                    "DELETE FROM \"{}\" WHERE entity_kind = ?1 AND entity_id = ?2",
                    self.member_table
                ),
                params![kind_key(kind), id],
            )?;
            conn.execute(&format!("DELETE FROM \"{table}\" WHERE {key} = ?1"), [id])?;
            Ok(())
        })
    }
}

impl GraphStorage for SQLiteGraphStorage {
    fn begin_write(&self) -> GraphStoreResult<Box<dyn GraphWriteTransaction>> {
        begin_graph_write(Arc::clone(&self.backend))
    }
    fn graph_names(&self) -> GraphStoreResult<Vec<String>> {
        self.sql(|conn| {
            let mut statement = conn.prepare_cached(&format!(
                "SELECT name FROM \"{}\" ORDER BY name",
                self.catalog_table
            ))?;
            let rows = statement.query_map([], |row| row.get(0))?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
    }
    fn has_graph(&self, graph: &str) -> GraphStoreResult<bool> {
        self.sql(|conn| {
            Ok(conn.query_row(
                &format!(
                    "SELECT EXISTS(SELECT 1 FROM \"{}\" WHERE name = ?1)",
                    self.catalog_table
                ),
                [graph],
                |row| row.get(0),
            )?)
        })
    }
    fn create_graph(&self, graph: &str) -> GraphStoreResult<()> {
        self.sql(|conn| {
            conn.execute(
                &format!(
                    "INSERT INTO \"{}\" (name) VALUES (?1) ON CONFLICT(name) DO NOTHING",
                    self.catalog_table
                ),
                [graph],
            )?;
            Ok(())
        })
    }
    fn delete_graph(&self, graph: &str) -> GraphStoreResult<()> {
        self.sql(|conn| {
            conn.execute(
                &format!("DELETE FROM \"{}\" WHERE name = ?1", self.catalog_table),
                [graph],
            )?;
            Ok(())
        })
    }
    fn registry(&self, graph: &str) -> GraphStoreResult<GraphLabelRegistry> {
        self.sql(|conn| {
            let json: String = conn.query_row(
                &format!(
                    "SELECT registry_json FROM \"{}\" WHERE name = ?1",
                    self.catalog_table
                ),
                [graph],
                |row| row.get(0),
            )?;
            Ok(serde_json::from_str(&json)?)
        })
    }
    fn save_registry(&self, graph: &str, registry: &GraphLabelRegistry) -> GraphStoreResult<()> {
        self.sql(|conn| {
            let json = serde_json::to_string(registry)?;
            conn.execute(
                &format!(
                    "UPDATE \"{}\" SET registry_json = ?1 WHERE name = ?2",
                    self.catalog_table
                ),
                params![json, graph],
            )?;
            Ok(())
        })
    }
    fn counter(&self, kind: GraphEntityKind) -> GraphStoreResult<Option<u64>> {
        self.metadata(&format!("next_{}_id", kind.as_str()))
            .map_err(|error| sqlite_graph_error(&error))?
            .map(|value| {
                value.parse().map_err(|error| {
                    GraphStoreError::CorruptGraph(format!("invalid graph id counter: {error}"))
                })
            })
            .transpose()
    }
    fn save_counter(&self, kind: GraphEntityKind, next: u64) -> GraphStoreResult<()> {
        self.save_metadata(&format!("next_{}_id", kind.as_str()), &next.to_string())
            .map_err(|error| sqlite_graph_error(&error))
    }
    fn vertex(&self, id: u64) -> GraphStoreResult<Option<Vertex>> {
        self.sql(|conn| {
            let encoded = encode_graph_id("vertex", id)?;
            let row = conn.query_row(
                &format!("SELECT label, properties_json, properties_format FROM \"{}\" WHERE vertex_id = ?1", self.vtx_table),
                [encoded], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, i64>(2)?)),
            ).optional()?;
            row.map(|(label, json, format)| Ok(Vertex { vertex_id: id, label, properties: decode_properties(&json, format)? })).transpose()
        })
    }
    fn edge(&self, id: u64) -> GraphStoreResult<Option<Edge>> {
        self.sql(|conn| {
            let encoded = encode_graph_id("edge", id)?;
            let row = conn.query_row(
                &format!("SELECT source_id, target_id, label, properties_json, properties_format FROM \"{}\" WHERE edge_id = ?1", self.edge_table),
                [encoded], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?, row.get::<_, String>(2)?, row.get::<_, String>(3)?, row.get::<_, i64>(4)?)),
            ).optional()?;
            row.map(|(source, target, label, json, format)| Ok(Edge {
                edge_id: id, source_id: decode_graph_id("source", source)?, target_id: decode_graph_id("target", target)?,
                label, properties: decode_properties(&json, format)?,
            })).transpose()
        })
    }
    fn save_vertex(&self, vertex: &Vertex) -> GraphStoreResult<()> {
        self.sql(|conn| {
            conn.execute(
                &format!("INSERT INTO \"{}\" (vertex_id, label, properties_json, properties_format) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(vertex_id) DO UPDATE SET label = excluded.label, properties_json = excluded.properties_json, properties_format = excluded.properties_format", self.vtx_table),
                params![encode_graph_id("vertex", vertex.vertex_id)?, vertex.label, serde_json::to_string(&vertex.properties)?, TAGGED_PROPERTIES_FORMAT],
            )?;
            Ok(())
        })
    }
    fn save_edge(&self, edge: &Edge) -> GraphStoreResult<()> {
        self.sql(|conn| {
            conn.execute(
                &format!("INSERT INTO \"{}\" (edge_id, source_id, target_id, label, properties_json, properties_format) VALUES (?1, ?2, ?3, ?4, ?5, ?6) ON CONFLICT(edge_id) DO UPDATE SET source_id = excluded.source_id, target_id = excluded.target_id, label = excluded.label, properties_json = excluded.properties_json, properties_format = excluded.properties_format", self.edge_table),
                params![encode_graph_id("edge", edge.edge_id)?, encode_graph_id("source", edge.source_id)?, encode_graph_id("target", edge.target_id)?, edge.label, serde_json::to_string(&edge.properties)?, TAGGED_PROPERTIES_FORMAT],
            )?;
            Ok(())
        })
    }
    fn delete_vertex(&self, id: u64) -> GraphStoreResult<()> {
        self.delete_entity(GraphEntityKind::Vertex, id)
    }
    fn delete_edge(&self, id: u64) -> GraphStoreResult<()> {
        self.delete_entity(GraphEntityKind::Edge, id)
    }
    fn ids(
        &self,
        filter: GraphEntityFilter<'_>,
        after: Option<u64>,
        limit: usize,
    ) -> GraphStoreResult<Vec<u64>> {
        if !(1..=MAX_GRAPH_ID_PAGE).contains(&limit) {
            return Err(GraphStoreError::InvalidQuery(format!(
                "graph page size must be between 1 and {MAX_GRAPH_ID_PAGE}"
            )));
        }
        let mut query = self.selection(filter)?;
        if let Some(after) = after {
            write!(query.from, " AND {} > ?", query.id).expect("write graph cursor predicate");
            query.values.push(SQLValue::Integer(
                encode_graph_id("cursor", after).map_err(|error| sqlite_graph_error(&error))?,
            ));
        }
        query
            .values
            .push(SQLValue::Integer(i64::try_from(limit).map_err(
                |error| GraphStoreError::InvalidQuery(error.to_string()),
            )?));
        let sql = format!(
            "SELECT {}, {} {} ORDER BY {} LIMIT ?",
            query.id, query.stored_id, query.from, query.id
        );
        self.sql(|conn| {
            let mut statement = conn.prepare_cached(&sql)?;
            let mut rows = statement.query(params_from_iter(query.values))?;
            let mut ids = Vec::new();
            while let Some(row) = rows.next()? {
                let id = decode_graph_id("entity", row.get(0)?)?;
                if row.get::<_, Option<i64>>(1)?.is_none() {
                    return Err(SQLiteError::StorageBackend(format!(
                        "graph {:?} references missing {} {id}",
                        filter.graph,
                        filter.kind.as_str()
                    )));
                }
                ids.push(id);
            }
            Ok(ids)
        })
    }
    fn count(&self, filter: GraphEntityFilter<'_>) -> GraphStoreResult<u64> {
        let query = self.selection(filter)?;
        self.sql(|conn| {
            let (count, stored): (i64, i64) = conn.query_row(
                &format!("SELECT count(*), count({}) {}", query.stored_id, query.from),
                params_from_iter(query.values),
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            if count != stored {
                return Err(SQLiteError::StorageBackend(format!(
                    "graph {:?} references missing {} records",
                    filter.graph,
                    filter.kind.as_str()
                )));
            }
            decode_graph_id("count", count)
        })
    }
    fn max_id(&self, kind: GraphEntityKind) -> GraphStoreResult<Option<u64>> {
        let (table, key) = self.entity_table(kind);
        self.sql(|conn| {
            let value: Option<i64> =
                conn.query_row(&format!("SELECT max({key}) FROM \"{table}\""), [], |row| {
                    row.get(0)
                })?;
            value
                .map(|value| decode_graph_id("entity", value))
                .transpose()
        })
    }
    fn memberships(&self, kind: GraphEntityKind, id: u64) -> GraphStoreResult<Vec<String>> {
        self.sql(|conn| {
            let mut statement = conn.prepare_cached(&format!(
                "SELECT graph FROM \"{}\" WHERE entity_kind = ?1 AND entity_id = ?2 ORDER BY graph",
                self.member_table
            ))?;
            let rows = statement.query_map(
                params![kind_key(kind), encode_graph_id("entity", id)?],
                |row| row.get(0),
            )?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
    }
    fn has_membership(
        &self,
        kind: GraphEntityKind,
        id: u64,
        graph: &str,
    ) -> GraphStoreResult<bool> {
        self.sql(|conn| Ok(conn.query_row(
            &format!("SELECT EXISTS(SELECT 1 FROM \"{}\" WHERE graph = ?1 AND entity_kind = ?2 AND entity_id = ?3)", self.member_table),
            params![graph, kind_key(kind), encode_graph_id("entity", id)?], |row| row.get(0),
        )?))
    }
    fn attach(&self, kind: GraphEntityKind, id: u64, graph: &str) -> GraphStoreResult<()> {
        self.sql(|conn| {
            conn.execute(&format!("INSERT INTO \"{}\" (graph, entity_kind, entity_id) VALUES (?1, ?2, ?3) ON CONFLICT DO NOTHING", self.member_table),
                params![graph, kind_key(kind), encode_graph_id("entity", id)?])?;
            Ok(())
        })
    }
    fn detach(&self, kind: GraphEntityKind, id: u64, graph: &str) -> GraphStoreResult<()> {
        self.sql(|conn| {
            conn.execute(
                &format!(
                    "DELETE FROM \"{}\" WHERE graph = ?1 AND entity_kind = ?2 AND entity_id = ?3",
                    self.member_table
                ),
                params![graph, kind_key(kind), encode_graph_id("entity", id)?],
            )?;
            Ok(())
        })
    }
}
