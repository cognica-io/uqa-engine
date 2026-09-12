//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Graph format ownership and retirement of legacy positional rows.

use super::{
    params, table_exists, OptionalExtension, SQLiteError, SQLiteInvertedIndex, SQLiteResult,
};

pub(super) const FIELD_TABLES: [&str; 4] = [
    "_occurrence_clusters",
    "_occurrence_documents",
    "_occurrence_lengths",
    "_occurrence_fields",
];
const LEGACY_TABLES: [&str; 5] = [
    "_postings",
    "_posting_clusters",
    "_posting_documents",
    "_doc_lengths",
    "_field_stats",
];

impl SQLiteInvertedIndex {
    pub(super) fn needs_source_rebuild_on(
        &self,
        conn: &rusqlite::Connection,
    ) -> SQLiteResult<bool> {
        let format: Option<String> = conn
            .query_row(
                "SELECT format FROM _occurrence_formats WHERE table_name = ?1",
                [&self.table],
                |row| row.get(0),
            )
            .optional()?;
        match format.as_deref() {
            Some("source-rebuild") => return Ok(true),
            Some("occurrences-v2") | None => {}
            Some(_) => {
                return Err(SQLiteError::StorageBackend(
                    "unsupported occurrence index format".into(),
                ))
            }
        }
        for table in LEGACY_TABLES {
            if table_exists(conn, table)? && has_rows(conn, table, &self.table)? {
                return Ok(true);
            }
        }
        if format.is_none() {
            for table in FIELD_TABLES {
                if has_rows(conn, table, &self.table)? {
                    return Err(SQLiteError::StorageBackend(
                        "occurrence index format marker is missing".into(),
                    ));
                }
            }
        }
        Ok(false)
    }

    pub(super) fn require_graph_format_on(&self, conn: &rusqlite::Connection) -> SQLiteResult<()> {
        if self.needs_source_rebuild_on(conn)? {
            return Err(SQLiteError::StorageBackend(
                "legacy positional data requires an atomic source rebuild".into(),
            ));
        }
        Ok(())
    }

    pub(super) fn require_graph_format(&self) -> SQLiteResult<()> {
        self.conn.with(|conn| self.require_graph_format_on(conn))
    }

    pub(super) fn publish_graph_format(&self, conn: &rusqlite::Connection) -> SQLiteResult<()> {
        conn.execute("INSERT INTO _occurrence_formats(table_name, format) VALUES (?1, 'occurrences-v2') ON CONFLICT(table_name) DO UPDATE SET format = excluded.format", [&self.table])?;
        Ok(())
    }

    pub(super) fn clear_index_on(&self, conn: &rusqlite::Connection) -> SQLiteResult<()> {
        for table in FIELD_TABLES.into_iter().chain(LEGACY_TABLES) {
            if table_exists(conn, table)? {
                conn.execute(
                    &format!("DELETE FROM {table} WHERE table_name = ?1"),
                    [&self.table],
                )?;
            }
        }
        self.publish_graph_format(conn)
    }
}

fn has_rows(conn: &rusqlite::Connection, table: &str, name: &str) -> SQLiteResult<bool> {
    Ok(conn
        .query_row(
            &format!("SELECT 1 FROM {table} WHERE table_name = ?1 LIMIT 1"),
            params![name],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}
