//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Storage-side graph identity, membership, and adjacency lookups.

use rusqlite::{params_from_iter, types::Value as SQLValue};
use std::fmt::Write as _;

use super::{
    decode_catalog_id, encode_catalog_id, Catalog, EdgeRow, OptionalExtension, Result, SQLiteError,
};
use uqa_storage::{GraphEntityFilter, GraphEntityKind, GraphVertexRow};

fn entity_table(kind: GraphEntityKind) -> (&'static str, &'static str) {
    match kind {
        GraphEntityKind::Vertex => ("_graph_vertices", "vertex_id"),
        GraphEntityKind::Edge => ("_graph_edges", "edge_id"),
    }
}

struct Selection {
    from: String,
    id: String,
    stored_id: String,
    values: Vec<SQLValue>,
}

fn selection(filter: GraphEntityFilter<'_>) -> Result<Selection> {
    filter
        .validate()
        .map_err(|error| SQLiteError::StorageBackend(error.to_string()))?;
    let (table, key) = entity_table(filter.kind);
    let mut values = Vec::new();
    let index = match (filter.kind, filter.source, filter.target, filter.label) {
        (GraphEntityKind::Edge, Some(_), _, _) => Some("_graph_edges_out"),
        (GraphEntityKind::Edge, _, Some(_), _) => Some("_graph_edges_in"),
        (GraphEntityKind::Edge, _, _, Some(_)) => Some("_graph_edges_label"),
        (GraphEntityKind::Vertex, _, _, Some(_)) => Some("_graph_vertices_label"),
        _ => None,
    };
    let (mut from, id) = if let Some(index) = index {
        // Adjacency and label reads start at the selective physical index,
        // never at the potentially much larger graph membership partition.
        (
            format!("FROM {table} AS e INDEXED BY {index} WHERE 1 = 1"),
            format!("e.{key}"),
        )
    } else if let Some(graph) = filter.graph {
        values.push(SQLValue::Text(graph.to_owned()));
        values.push(SQLValue::Text(filter.kind.as_str().to_owned()));
        (format!("FROM _graph_membership AS m LEFT JOIN {table} AS e ON e.{key} = m.entity_id WHERE m.graph_name = ? AND m.entity_type = ?"), "m.entity_id".to_owned())
    } else {
        (format!("FROM {table} AS e WHERE 1 = 1"), format!("e.{key}"))
    };
    if let Some(label) = filter.label {
        from.push_str(" AND e.label = ?");
        values.push(SQLValue::Text(label.to_owned()));
    }
    if let Some(source) = filter.source {
        from.push_str(" AND e.source_id = ?");
        values.push(SQLValue::Integer(encode_catalog_id("edge source", source)?));
    }
    if let Some(target) = filter.target {
        from.push_str(" AND e.target_id = ?");
        values.push(SQLValue::Integer(encode_catalog_id("edge target", target)?));
    }
    if index.is_some() {
        if let Some(graph) = filter.graph {
            write!(from, " AND EXISTS (SELECT 1 FROM _graph_membership AS m WHERE m.entity_type = ? AND m.entity_id = e.{key} AND m.graph_name = ?)").expect("write graph membership predicate");
            values.push(SQLValue::Text(filter.kind.as_str().to_owned()));
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

impl Catalog {
    pub fn named_graph_exists(&self, name: &str) -> Result<bool> {
        self.conn.with(|conn| {
            Ok(conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM _named_graphs WHERE name = ?1)",
                [name],
                |row| row.get(0),
            )?)
        })
    }

    pub fn graph_vertex(&self, id: u64) -> Result<Option<GraphVertexRow>> {
        let encoded = encode_catalog_id("vertex", id)?;
        self.conn.with(|conn| {
            Ok(conn
                .query_row(
                    "SELECT label, properties_json FROM _graph_vertices WHERE vertex_id = ?1",
                    [encoded],
                    |row| {
                        Ok(GraphVertexRow {
                            vertex_id: id,
                            label: row.get(0)?,
                            properties_json: row.get(1)?,
                        })
                    },
                )
                .optional()?)
        })
    }

    pub fn graph_edge(&self, id: u64) -> Result<Option<EdgeRow>> {
        let encoded = encode_catalog_id("edge", id)?;
        self.conn.with(|conn| {
            let row = conn.query_row(
                "SELECT source_id, target_id, label, properties_json FROM _graph_edges WHERE edge_id = ?1",
                [encoded],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?, row.get::<_, String>(2)?, row.get::<_, String>(3)?)),
            ).optional()?;
            row.map(|(source, target, label, properties_json)| Ok(EdgeRow {
                edge_id: id,
                source_id: decode_catalog_id("edge source", source)?,
                target_id: decode_catalog_id("edge target", target)?,
                label, properties_json,
            })).transpose()
        })
    }

    pub fn graph_entity_ids(
        &self,
        filter: GraphEntityFilter<'_>,
        after: Option<u64>,
        limit: usize,
    ) -> Result<Vec<u64>> {
        uqa_storage::catalog::validate_graph_page(limit)
            .map_err(|error| SQLiteError::StorageBackend(error.to_string()))?;
        let mut query = selection(filter)?;
        if let Some(after) = after {
            write!(query.from, " AND {} > ?", query.id).expect("write graph cursor predicate");
            query.values.push(SQLValue::Integer(encode_catalog_id(
                "graph scan cursor",
                after,
            )?));
        }
        query.values.push(SQLValue::Integer(
            i64::try_from(limit).map_err(|error| SQLiteError::StorageBackend(error.to_string()))?,
        ));
        let sql = format!(
            "SELECT {}, {} {} ORDER BY {} LIMIT ?",
            query.id, query.stored_id, query.from, query.id
        );
        self.conn.with(|conn| {
            let mut statement = conn.prepare_cached(&sql)?;
            let mut rows = statement.query(params_from_iter(query.values))?;
            let mut ids = Vec::new();
            while let Some(row) = rows.next()? {
                let id = decode_catalog_id("graph entity", row.get(0)?)?;
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

    pub fn graph_entity_count(&self, filter: GraphEntityFilter<'_>) -> Result<u64> {
        let query = selection(filter)?;
        let sql = format!("SELECT count(*), count({}) {}", query.stored_id, query.from);
        self.conn.with(|conn| {
            let (count, stored): (i64, i64) =
                conn.query_row(&sql, params_from_iter(query.values), |row| {
                    Ok((row.get(0)?, row.get(1)?))
                })?;
            if count != stored {
                return Err(SQLiteError::StorageBackend(format!(
                    "graph {:?} references missing {} records",
                    filter.graph,
                    filter.kind.as_str()
                )));
            }
            u64::try_from(count).map_err(|error| SQLiteError::StorageBackend(error.to_string()))
        })
    }

    pub fn graph_entity_max_id(&self, kind: GraphEntityKind) -> Result<Option<u64>> {
        let (table, key) = entity_table(kind);
        self.conn.with(|conn| {
            let id: Option<i64> =
                conn.query_row(&format!("SELECT max({key}) FROM {table}"), [], |row| {
                    row.get(0)
                })?;
            id.map(|id| decode_catalog_id("graph entity", id))
                .transpose()
        })
    }

    pub fn graph_entity_memberships(&self, kind: GraphEntityKind, id: u64) -> Result<Vec<String>> {
        let encoded = encode_catalog_id("graph entity", id)?;
        self.conn.with(|conn| {
            let mut statement = conn.prepare_cached("SELECT graph_name FROM _graph_membership WHERE entity_type = ?1 AND entity_id = ?2 ORDER BY graph_name")?;
            let rows = statement.query_map(rusqlite::params![kind.as_str(), encoded], |row| row.get(0))?;
            Ok(rows.collect::<std::result::Result<_, _>>()?)
        })
    }

    pub fn graph_has_membership(
        &self,
        kind: GraphEntityKind,
        id: u64,
        graph: &str,
    ) -> Result<bool> {
        let encoded = encode_catalog_id("graph entity", id)?;
        self.conn.with(|conn| Ok(conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM _graph_membership WHERE entity_type = ?1 AND entity_id = ?2 AND graph_name = ?3)",
            rusqlite::params![kind.as_str(), encoded, graph], |row| row.get(0),
        )?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adjacency_and_label_pages_start_at_the_selective_entity_index() {
        let connection = crate::ManagedConnection::open_in_memory().unwrap();
        Catalog::open(connection.clone()).unwrap();
        for (kind, source, target, label, expected) in [
            (
                GraphEntityKind::Edge,
                Some(17),
                None,
                Some("LINK"),
                "_graph_edges_out",
            ),
            (
                GraphEntityKind::Edge,
                Some(17),
                None,
                None,
                "_graph_edges_out",
            ),
            (
                GraphEntityKind::Edge,
                None,
                Some(17),
                Some("LINK"),
                "_graph_edges_in",
            ),
            (
                GraphEntityKind::Edge,
                None,
                None,
                Some("LINK"),
                "_graph_edges_label",
            ),
            (
                GraphEntityKind::Vertex,
                None,
                None,
                Some("Item"),
                "_graph_vertices_label",
            ),
        ] {
            let query = selection(GraphEntityFilter {
                kind,
                graph: Some("test_graph"),
                source,
                target,
                label,
            })
            .unwrap();
            let sql = format!(
                "EXPLAIN QUERY PLAN SELECT {} {} ORDER BY {} LIMIT 256",
                query.id, query.from, query.id
            );
            let plan: Vec<String> = connection
                .with(|conn| {
                    let mut statement = conn.prepare(&sql)?;
                    let rows =
                        statement.query_map(params_from_iter(query.values), |row| row.get(3))?;
                    Ok(rows.collect::<std::result::Result<_, _>>()?)
                })
                .unwrap();
            assert!(
                plan.iter()
                    .any(|step| step.contains("SEARCH e USING") && step.contains(expected)),
                "{plan:?}"
            );
            assert!(!plan.iter().any(|step| step.contains("SCAN m")), "{plan:?}");
        }
    }
}
