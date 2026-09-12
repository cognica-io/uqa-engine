//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Separate lossless occurrence storage from positional data that needs source reconstruction.

use super::super::super::{params, table_exists, Catalog, Result};
use uqa_storage::RelationIdentity;

pub(super) fn migrate(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS _occurrence_clusters (
        table_name TEXT NOT NULL, field TEXT NOT NULL, term BLOB NOT NULL,
        cluster_id INTEGER NOT NULL, posting_count INTEGER NOT NULL CHECK (posting_count > 0),
        score_blob BLOB NOT NULL, positions_blob BLOB NOT NULL,
        PRIMARY KEY (table_name, field, term, cluster_id)
    ) WITHOUT ROWID;
    CREATE TABLE IF NOT EXISTS _occurrence_documents (
        table_name TEXT NOT NULL, doc_id INTEGER NOT NULL, field TEXT NOT NULL,
        terms_blob BLOB NOT NULL, metadata_blob BLOB NOT NULL,
        PRIMARY KEY (table_name, doc_id, field)
    ) WITHOUT ROWID;
    CREATE TABLE IF NOT EXISTS _occurrence_lengths (
        table_name TEXT NOT NULL, doc_id INTEGER NOT NULL, field TEXT NOT NULL,
        length INTEGER NOT NULL, PRIMARY KEY (table_name, doc_id, field)
    ) WITHOUT ROWID;
    CREATE TABLE IF NOT EXISTS _occurrence_fields (
        table_name TEXT NOT NULL, field TEXT NOT NULL, revision BLOB NOT NULL,
        doc_count INTEGER NOT NULL, total_length INTEGER NOT NULL,
        PRIMARY KEY (table_name, field)
    ) WITHOUT ROWID;
    CREATE TABLE IF NOT EXISTS _occurrence_formats (
        table_name TEXT PRIMARY KEY, format TEXT NOT NULL
    ) WITHOUT ROWID;",
    )?;
    let mut tables = std::collections::BTreeSet::new();
    let mut statement =
        conn.prepare("SELECT schema_name, relation_name, fts_fields FROM _tables")?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    for row in rows {
        let (schema, name, fields) = row?;
        if !serde_json::from_str::<Vec<String>>(&fields)?.is_empty() {
            tables.insert(RelationIdentity::new(schema, name).qualified_name());
        }
    }
    drop(statement);
    for table in [
        "_postings",
        "_posting_clusters",
        "_posting_documents",
        "_doc_lengths",
        "_field_stats",
    ] {
        if table_exists(conn, table)? {
            let mut statement =
                conn.prepare(&format!("SELECT DISTINCT table_name FROM {table}"))?;
            for row in statement.query_map([], |row| row.get::<_, String>(0))? {
                tables.insert(row?);
            }
        }
    }
    for table in tables {
        conn.execute("INSERT INTO _occurrence_formats(table_name, format) VALUES (?1, 'source-rebuild') ON CONFLICT(table_name) DO NOTHING", params![table])?;
    }
    Catalog::install_cache_revision_tracking(conn)
}
