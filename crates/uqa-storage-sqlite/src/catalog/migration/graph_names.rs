//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retire the Python graph-name alias without treating catalog entities as standalone graphs.

use super::super::{Catalog, Result, SQLiteError};

fn require_columns(
    connection: &rusqlite::Connection,
    table: &str,
    expected: &[(&str, &str, i64)],
) -> Result<()> {
    let mut statement = connection.prepare(&format!("PRAGMA table_info({table})"))?;
    let mut rows = statement.query([])?;
    for &(name, kind, primary) in expected {
        let matches = match rows.next()? {
            Some(row) => {
                row.get_ref(1)?
                    .as_str()
                    .is_ok_and(|value| value.eq_ignore_ascii_case(name))
                    && row
                        .get_ref(2)?
                        .as_str()
                        .is_ok_and(|value| value.eq_ignore_ascii_case(kind))
                    && row.get::<_, i64>(5)? == primary
            }
            None => false,
        };
        if !matches {
            return Err(SQLiteError::StorageBackend(format!(
                "legacy graph catalog requires the native {table} column layout"
            )));
        }
    }
    if rows.next()?.is_some() {
        return Err(SQLiteError::StorageBackend(format!(
            "legacy graph catalog has an unknown column in {table}"
        )));
    }
    Ok(())
}

impl Catalog {
    pub(crate) fn promote_legacy_graph_catalog(connection: &rusqlite::Connection) -> Result<()> {
        let legacy: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('_graph_catalog') WHERE cid = 0 AND lower(name) = 'graph_name')",
            [],
            |row| row.get(0),
        )?;
        if !legacy {
            return Ok(());
        }
        require_columns(connection, "_graph_catalog", &[("graph_name", "TEXT", 1)])?;
        require_columns(connection, "_named_graphs", &[("name", "TEXT", 1)])?;
        require_columns(
            connection,
            "_graph_membership",
            &[
                ("entity_type", "TEXT", 1),
                ("entity_id", "INTEGER", 2),
                ("graph_name", "TEXT", 3),
            ],
        )?;
        let invalid: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM _graph_catalog WHERE typeof(graph_name) != 'text')",
            [],
            |row| row.get(0),
        )?;
        if invalid {
            return Err(SQLiteError::StorageBackend(
                "legacy graph catalog contains a non-text graph name".into(),
            ));
        }
        connection.execute_batch(
            "INSERT INTO _named_graphs(name) SELECT graph_name FROM _graph_catalog WHERE true ON CONFLICT(name) DO NOTHING;
             DROP TABLE _graph_catalog;",
        )?;
        Ok(())
    }
}
