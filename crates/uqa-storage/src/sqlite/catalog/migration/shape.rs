//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog shape inspection and idempotent repair after ordered migrations.

use super::super::{params, quote_sql_identifier, Catalog, Result};

impl Catalog {
    pub(super) fn ensure_fts_storage_shape(conn: &rusqlite::Connection) -> Result<bool> {
        let doc_lengths = Self::table_columns(conn, "_doc_lengths")?;
        let doc_lengths_ok = doc_lengths
            .as_ref()
            .is_some_and(|cols| cols.contains_key("field") && cols.contains_key("length"));
        if doc_lengths_ok && super::steps::v22::clustered_posting_tables_have_current_shape(conn)? {
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS _field_stats (
                    table_name   TEXT NOT NULL,
                    field        TEXT NOT NULL,
                    total_length INTEGER NOT NULL DEFAULT 0,
                    PRIMARY KEY (table_name, field)
                );",
            )?;
            return Ok(false);
        }

        conn.execute_batch(
            "
            DROP TABLE IF EXISTS _postings;
            DROP TABLE IF EXISTS _posting_clusters;
            DROP TABLE IF EXISTS _posting_documents;
            DROP TABLE IF EXISTS _doc_lengths;
            DROP TABLE IF EXISTS _field_stats;

            CREATE TABLE _posting_clusters (
                table_name     TEXT NOT NULL,
                field          TEXT NOT NULL,
                term           TEXT NOT NULL,
                cluster_id     INTEGER NOT NULL,
                posting_count  INTEGER NOT NULL CHECK (posting_count > 0),
                score_blob     BLOB NOT NULL,
                positions_blob BLOB NOT NULL,
                PRIMARY KEY (table_name, field, term, cluster_id)
            ) WITHOUT ROWID;

            CREATE TABLE _posting_documents (
                table_name TEXT NOT NULL,
                doc_id     INTEGER NOT NULL,
                field      TEXT NOT NULL,
                terms_blob BLOB NOT NULL,
                PRIMARY KEY (table_name, doc_id, field)
            ) WITHOUT ROWID;

            CREATE TABLE IF NOT EXISTS _doc_lengths (
                table_name TEXT NOT NULL,
                doc_id     INTEGER NOT NULL,
                field      TEXT NOT NULL,
                length     INTEGER NOT NULL,
                PRIMARY KEY (table_name, doc_id, field)
            );

            CREATE TABLE IF NOT EXISTS _field_stats (
                table_name   TEXT NOT NULL,
                field        TEXT NOT NULL,
                total_length INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (table_name, field)
            );
            ",
        )?;
        Ok(true)
    }

    pub(super) fn table_columns(
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
