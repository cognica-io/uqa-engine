//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Storage-owned, rollback-safe cache generations for every catalog session.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use super::{quote_sql_identifier, Catalog, Result, SQLiteError};
use crate::CatalogCacheRevisions;

impl Catalog {
    pub(super) fn install_cache_revision_tracking(conn: &rusqlite::Connection) -> Result<()> {
        let tables = conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND substr(name, 1, 1) = '_' AND name NOT IN ('_cache_revisions', '_graph_path_pairs', '_graph_path_index_state') ORDER BY name")?
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        for table in tables {
            let columns = Self::table_columns(conn, &table)?.unwrap_or_default();
            for (event, images) in [
                ("INSERT", &["NEW"][..]),
                ("DELETE", &["OLD"][..]),
                ("UPDATE", &["OLD", "NEW"][..]),
            ] {
                let trigger = quote_sql_identifier(&format!("uqa_cache_{table}_{event}"));
                let mut body = String::new();
                for image in images {
                    if matches!(table.as_str(), "_graph_vertices" | "_graph_edges") {
                        let (entity_type, id) = if table == "_graph_vertices" {
                            ("vertex", "vertex_id")
                        } else {
                            ("edge", "edge_id")
                        };
                        // A global entity can belong to several graphs. Its
                        // property changes invalidate exactly those owners.
                        write!(
                            body,
                            "INSERT INTO _cache_revisions(kind, name, generation) \
                             SELECT 'graph', graph_name, 1 FROM _graph_membership \
                             WHERE entity_type = '{entity_type}' AND entity_id = {image}.{id} \
                             ON CONFLICT(kind, name) DO UPDATE SET generation = generation + 1;"
                        )
                        .expect("write graph revision trigger");
                    } else {
                        let (kind, name) =
                            revision_scope(&table, columns.contains_key("table_name"), image);
                        write!(body,
                            "INSERT INTO _cache_revisions(kind, name, generation) VALUES ({kind}, {name}, 1) \
                             ON CONFLICT(kind, name) DO UPDATE SET generation = generation + 1;"
                        ).expect("writing a cache revision trigger to a String cannot fail");
                    }
                }
                conn.execute_batch(&format!(
                    "CREATE TRIGGER IF NOT EXISTS {trigger} AFTER {event} ON {} BEGIN {body} END;",
                    quote_sql_identifier(&table),
                ))?;
            }
        }
        Ok(())
    }

    pub fn cache_revisions(&self) -> Result<CatalogCacheRevisions> {
        self.conn.with(|conn| {
            let mut revisions = CatalogCacheRevisions {
                graphs: Some(BTreeMap::new()),
                storage_schema: u64::from(conn.pragma_query_value(
                    None,
                    "schema_version",
                    |row| row.get::<_, u32>(0),
                )?),
                ..CatalogCacheRevisions::default()
            };
            let mut statement = conn.prepare_cached(
                "SELECT kind, name, generation FROM _cache_revisions ORDER BY kind, name",
            )?;
            let mut rows = statement.query([])?;
            while let Some(row) = rows.next()? {
                let kind: String = row.get(0)?;
                let name: String = row.get(1)?;
                let generation = u64::try_from(row.get::<_, i64>(2)?)
                    .map_err(|_| SQLiteError::StorageBackend("negative cache revision".into()))?;
                match kind.as_str() {
                    "catalog" => revisions.table_catalog = generation,
                    "registry" => revisions.registries = generation,
                    "graph" => {
                        revisions
                            .graphs
                            .as_mut()
                            .expect("graph tracking enabled")
                            .insert(name, generation);
                    }
                    "data" => {
                        revisions.table_data.insert(name, generation);
                    }
                    "statistics" => {
                        revisions.column_statistics.insert(name, generation);
                    }
                    "maintenance" => {
                        revisions.statistics_maintenance.insert(name, generation);
                    }
                    _ => {
                        return Err(SQLiteError::StorageBackend(format!(
                            "unknown cache revision kind `{kind}`"
                        )))
                    }
                }
            }
            Ok(revisions)
        })
    }
}

fn revision_scope(table: &str, has_table_name: bool, image: &str) -> (String, String) {
    match table {
        "_tables" => ("'catalog'".into(), "''".into()),
        "_column_stats" => ("'statistics'".into(), format!("{image}.table_name")),
        "_named_graphs" => ("'graph'".into(), format!("{image}.name")),
        "_graph_membership" => ("'graph'".into(), format!("{image}.graph_name")),
        "_metadata" => {
            let key = format!("{image}.key");
            let maintenance = "uqa.statistics.maintenance.v1:";
            let next_id = "uqa.table_next_id.v1:";
            let graph_labels = "graph_label_registry::";
            (
                format!("CASE WHEN substr({key}, 1, {}) = '{maintenance}' THEN 'maintenance' WHEN substr({key}, 1, {}) = '{next_id}' THEN 'data' WHEN substr({key}, 1, {}) = '{graph_labels}' THEN 'graph' ELSE 'registry' END", maintenance.len(), next_id.len(), graph_labels.len()),
                format!("CASE WHEN substr({key}, 1, {}) = '{maintenance}' THEN substr({key}, {}) WHEN substr({key}, 1, {}) = '{next_id}' THEN substr({key}, {}) WHEN substr({key}, 1, {}) = '{graph_labels}' THEN substr({key}, {}) ELSE '' END", maintenance.len(), maintenance.len() + 1, next_id.len(), next_id.len() + 1, graph_labels.len(), graph_labels.len() + 1),
            )
        }
        // These rows define access paths, not the data stored in those paths.
        "_table_field_analyzers" | "_catalog_indexes" | "_btree_indexes" => {
            ("'registry'".into(), "''".into())
        }
        _ if has_table_name => ("'data'".into(), format!("{image}.table_name")),
        _ => ("'registry'".into(), "''".into()),
    }
}
