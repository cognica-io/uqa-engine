//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Storage-owned, rollback-safe cache generations for every catalog session.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use super::{quote_sql_identifier, Catalog, OptionalExtension, Result, SQLiteError};
use crate::mvcc::native::NativeSnapshot;
use uqa_storage::CatalogCacheRevisions;

const METADATA_SCOPES: [(&str, &str); 3] = [
    ("uqa.statistics.maintenance.v1:", "maintenance"),
    ("uqa.table_next_id.v1:", "data"),
    ("graph_label_registry::", "graph"),
];

pub(super) fn metadata_scope(name: &str) -> Option<(&'static str, &str)> {
    if matches!(
        name,
        "graph_identifier_generation" | "graph_identifier_data_revision"
    ) || name.starts_with("graph_definition_data_revision::")
    {
        return None;
    }
    for (prefix, kind) in METADATA_SCOPES {
        if let Some(name) = name.strip_prefix(prefix) {
            return Some((kind, name));
        }
    }
    Some(("registry", ""))
}

impl Catalog {
    pub(super) fn install_cache_revision_tracking(conn: &rusqlite::Connection) -> Result<()> {
        let tables = conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND substr(name, 1, 1) = '_' AND name NOT GLOB '_uqa_mvcc_*' AND name NOT IN ('_cache_revisions', '_graph_path_pairs', '_graph_path_index_state') ORDER BY name")?
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        for table in tables {
            let columns = Self::table_columns(conn, &table)?.unwrap_or_default();
            for event in ["INSERT", "DELETE", "UPDATE"] {
                let (_, sql) =
                    Self::cache_revision_trigger(&table, columns.contains_key("table_name"), event);
                conn.execute_batch(&sql.replacen(
                    "CREATE TRIGGER",
                    "CREATE TRIGGER IF NOT EXISTS",
                    1,
                ))?;
            }
        }
        Ok(())
    }

    pub(crate) fn cache_revision_trigger(
        table: &str,
        has_table_name: bool,
        event: &str,
    ) -> (String, String) {
        cache_trigger(table, has_table_name, event, true, true)
    }

    /// Upgrade known metadata trigger encodings and exclude internal allocation state without altering data or revision history. The caller owns the schema transaction.
    pub(crate) fn upgrade_metadata_cache_triggers(conn: &rusqlite::Connection) -> Result<()> {
        for event in ["INSERT", "DELETE", "UPDATE"] {
            let (name, expected) = Self::cache_revision_trigger("_metadata", false, event);
            let current: Option<String> = conn
                .query_row(
                    "SELECT sql FROM sqlite_schema WHERE type = 'trigger' AND name = ?1",
                    [&name],
                    |row| row.get(0),
                )
                .optional()?;
            if current.as_deref() == Some(expected.as_str()) {
                continue;
            }
            let previous = cache_trigger("_metadata", false, event, false, false).1;
            let binary = cache_trigger("_metadata", false, event, true, false).1;
            if current.as_deref() != Some(previous.as_str())
                && current.as_deref() != Some(binary.as_str())
            {
                return Err(SQLiteError::StorageBackend(
                    "missing or changed metadata cache trigger".into(),
                ));
            }
            conn.execute_batch(&format!(
                "DROP TRIGGER {}; {expected}",
                quote_sql_identifier(&name)
            ))?;
        }
        Ok(())
    }

    pub fn cache_revisions(&self) -> Result<CatalogCacheRevisions> {
        if let Some(revisions) = self.read_native(NativeSnapshot::cache_revisions)? {
            return Ok(revisions);
        }
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
                *revision_slot(&mut revisions, &kind, &name)? = generation;
            }
            Ok(revisions)
        })
    }
}

pub(super) fn revision_slot<'a>(
    revisions: &'a mut CatalogCacheRevisions,
    kind: &str,
    name: &str,
) -> Result<&'a mut u64> {
    Ok(match kind {
        "catalog" => &mut revisions.table_catalog,
        "registry" => &mut revisions.registries,
        "graph" => revisions
            .graphs
            .as_mut()
            .expect("graph tracking enabled")
            .entry(name.into())
            .or_default(),
        "data" => revisions.table_data.entry(name.into()).or_default(),
        "statistics" => revisions.column_statistics.entry(name.into()).or_default(),
        "maintenance" => revisions
            .statistics_maintenance
            .entry(name.into())
            .or_default(),
        _ => {
            return Err(SQLiteError::StorageBackend(format!(
                "unknown cache revision kind `{kind}`"
            )))
        }
    })
}

fn cache_trigger(
    table: &str,
    has_table_name: bool,
    event: &str,
    binary_names: bool,
    exclude_allocations: bool,
) -> (String, String) {
    let name = format!("uqa_cache_{table}_{event}");
    let images: &[&str] = match event {
        "INSERT" => &["NEW"],
        "DELETE" => &["OLD"],
        _ => &["OLD", "NEW"],
    };
    let mut body = String::new();
    for image in images {
        if matches!(table, "_graph_vertices" | "_graph_edges") {
            let (entity_type, id) = if table == "_graph_vertices" {
                ("vertex", "vertex_id")
            } else {
                ("edge", "edge_id")
            };
            write!(
                body,
                "INSERT INTO _cache_revisions(kind, name, generation) \
                         SELECT 'graph', graph_name, 1 FROM _graph_membership \
                         WHERE entity_type = '{entity_type}' AND entity_id = {image}.{id} \
                         ON CONFLICT(kind, name) DO UPDATE SET generation = generation + 1;"
            )
            .expect("write graph revision trigger");
        } else {
            let (kind, name) = revision_scope(table, has_table_name, image, binary_names);
            let values = if table == "_metadata" && exclude_allocations {
                let prefix = "graph_definition_data_revision::";
                format!("SELECT {kind}, {name}, 1 WHERE {image}.key NOT IN ('graph_identifier_generation', 'graph_identifier_data_revision') AND substr(CAST({image}.key AS BLOB), 1, {}) != CAST('{prefix}' AS BLOB)", prefix.len())
            } else {
                format!("VALUES ({kind}, {name}, 1)")
            };
            write!(
                body,
                "INSERT INTO _cache_revisions(kind, name, generation) {values} \
                         ON CONFLICT(kind, name) DO UPDATE SET generation = generation + 1;"
            )
            .expect("write catalog revision trigger");
        }
    }
    let sql = format!(
        "CREATE TRIGGER {} AFTER {event} ON {} BEGIN {body} END",
        quote_sql_identifier(&name),
        quote_sql_identifier(table)
    );
    (name, sql)
}

fn revision_scope(
    table: &str,
    has_table_name: bool,
    image: &str,
    binary_names: bool,
) -> (String, String) {
    match table {
        "_tables" => ("'catalog'".into(), "''".into()),
        "_column_stats" => ("'statistics'".into(), format!("{image}.table_name")),
        "_named_graphs" => ("'graph'".into(), format!("{image}.name")),
        "_graph_membership" => ("'graph'".into(), format!("{image}.graph_name")),
        "_metadata" => {
            let key = format!("{image}.key");
            let [(maintenance, _), (next_id, _), (graph_labels, _)] = METADATA_SCOPES;
            let suffix = |length: usize| {
                if binary_names {
                    format!("CAST(substr(CAST({key} AS BLOB), {}) AS TEXT)", length + 1)
                } else {
                    format!("substr({key}, {})", length + 1)
                }
            };
            (
                format!("CASE WHEN substr({key}, 1, {}) = '{maintenance}' THEN 'maintenance' WHEN substr({key}, 1, {}) = '{next_id}' THEN 'data' WHEN substr({key}, 1, {}) = '{graph_labels}' THEN 'graph' ELSE 'registry' END", maintenance.len(), next_id.len(), graph_labels.len()),
                format!("CASE WHEN substr({key}, 1, {}) = '{maintenance}' THEN {} WHEN substr({key}, 1, {}) = '{next_id}' THEN {} WHEN substr({key}, 1, {}) = '{graph_labels}' THEN {} ELSE '' END", maintenance.len(), suffix(maintenance.len()), next_id.len(), suffix(next_id.len()), graph_labels.len(), suffix(graph_labels.len())),
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
