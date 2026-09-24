//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retire empty legacy search layouts after source reconstruction, before native record import.

use super::super::{table_exists, Catalog, Result, SQLiteError};
use super::steps::v22;

impl Catalog {
    pub(crate) fn prepare_native_fts_sources(conn: &rusqlite::Connection) -> Result<()> {
        let mut recreated = Vec::new();
        // Positional source reconstruction belongs to the index restore above this boundary. Import must never discard source rows that still need it.
        if table_exists(conn, "_postings")? {
            require_empty(conn, "_postings")?;
            conn.execute_batch("DROP TABLE _postings")?;
        }
        if Self::table_columns(conn, "_doc_lengths")?.is_some_and(|columns| {
            columns
                == [
                    ("table_name".into(), "TEXT".into()),
                    ("doc_id".into(), "INTEGER".into()),
                    ("lengths".into(), "TEXT".into()),
                ]
                .into_iter()
                .collect()
        }) {
            require_empty(conn, "_doc_lengths")?;
            conn.execute_batch("DROP TABLE _doc_lengths")?;
        }
        for table in ["_doc_lengths", "_field_stats"] {
            if !table_exists(conn, table)? {
                recreated.push(table);
            }
        }
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS _doc_lengths (
                table_name TEXT NOT NULL, doc_id INTEGER NOT NULL,
                field TEXT NOT NULL, length INTEGER NOT NULL,
                PRIMARY KEY (table_name, doc_id, field)
            );
            CREATE TABLE IF NOT EXISTS _field_stats (
                table_name TEXT NOT NULL, field TEXT NOT NULL,
                total_length INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (table_name, field)
            );",
        )?;
        if !v22::clustered_posting_tables_have_current_shape(conn)? {
            for table in ["_posting_clusters", "_posting_documents"] {
                if table_exists(conn, table)? {
                    require_empty(conn, table)?;
                }
            }
            v22::migrate(conn)?;
            recreated.extend(["_posting_clusters", "_posting_documents"]);
        }
        // Replacing retired tables removes their triggers. Restore only those definitions; an unrelated missing or changed guard must still fail native format validation.
        for table in recreated {
            for event in ["INSERT", "DELETE", "UPDATE"] {
                conn.execute_batch(&Self::cache_revision_trigger(table, true, event).1)?;
            }
        }
        Ok(())
    }
}

fn require_empty(conn: &rusqlite::Connection, table: &str) -> Result<()> {
    if conn.query_row(
        &format!("SELECT EXISTS(SELECT 1 FROM {table})"),
        [],
        |row| row.get::<_, bool>(0),
    )? {
        return Err(SQLiteError::StorageBackend(format!(
            "legacy search table {table} requires source reconstruction before native import"
        )));
    }
    Ok(())
}
