//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog shape inspection and idempotent repair after ordered migrations.

use super::super::{params, quote_sql_identifier, Catalog, Result};

impl Catalog {
    pub(in crate::catalog) fn table_columns(
        conn: &rusqlite::Connection,
        table_name: &str,
    ) -> Result<Option<std::collections::BTreeMap<String, String>>> {
        let exists: bool = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name = ?1",
            params![table_name],
            |r| r.get::<_, i64>(0),
        )? > 0;
        if !exists {
            return Ok(None);
        }
        let mut stmt = conn.prepare(&format!(
            "PRAGMA table_info({})",
            quote_sql_identifier(table_name)
        ))?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(1)?, r.get::<_, String>(2)?)))?;
        let mut out = std::collections::BTreeMap::new();
        for row in rows {
            let (name, ty) = row?;
            out.insert(name, ty);
        }
        Ok(Some(out))
    }

    pub(super) fn ensure_column_stats_shape(conn: &rusqlite::Connection) -> Result<()> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS _column_stats (
                table_name      TEXT NOT NULL,
                column_name     TEXT NOT NULL,
                distinct_count  INTEGER NOT NULL,
                null_count      INTEGER NOT NULL,
                min_value       TEXT,
                max_value       TEXT,
                row_count       INTEGER NOT NULL,
                histogram       TEXT NOT NULL DEFAULT '[]',
                mcv_values      TEXT NOT NULL DEFAULT '[]',
                mcv_frequencies TEXT NOT NULL DEFAULT '[]',
                PRIMARY KEY (table_name, column_name)
            );",
        )?;
        let mut stmt = conn.prepare("PRAGMA table_info(_column_stats)")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
        let mut cols = std::collections::BTreeSet::new();
        for row in rows {
            cols.insert(row?);
        }
        for col in ["histogram", "mcv_values", "mcv_frequencies"] {
            if !cols.contains(col) {
                conn.execute(
                    &format!(
                        "ALTER TABLE _column_stats ADD COLUMN {col} TEXT NOT NULL DEFAULT '[]'"
                    ),
                    [],
                )?;
            }
        }
        Ok(())
    }
}
