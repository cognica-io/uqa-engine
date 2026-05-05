//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog: schema versioning, migrations, and persisted table metadata.
//!
//! The catalog owns a single `_meta` table that records the schema version
//! plus a `_tables` table holding per-table analyzer config and the lists
//! of FTS / vector fields registered on each table. Concrete data tables
//! (`_documents`, `_postings`, `_vectors`, ...) are created by their
//! respective stores.

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::sqlite::connection::{ManagedConnection, Result};

/// Bump this every time a migration is added.
pub const CURRENT_SCHEMA_VERSION: u32 = 3;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableSchema {
    pub name: String,
    pub analyzer_json: String,
    pub fts_fields: Vec<String>,
    pub vector_fields: Vec<VectorFieldSchema>,
    /// Serialized `Vec<uqa_sql::ast::ColumnDef>` capturing the schema
    /// columns (name, type, `auto_increment`, flags). Empty for
    /// tables created by the legacy code path before column tracking
    /// existed.
    #[serde(default)]
    pub columns_json: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VectorFieldSchema {
    pub field: String,
    pub dimensions: u32,
}

pub struct Catalog {
    conn: ManagedConnection,
}

impl Catalog {
    /// Open (or create) the catalog and run any pending migrations.
    pub fn open(conn: ManagedConnection) -> Result<Self> {
        let cat = Self { conn };
        cat.run_migrations()?;
        Ok(cat)
    }

    pub fn connection(&self) -> ManagedConnection {
        self.conn.clone()
    }

    fn run_migrations(&self) -> Result<()> {
        self.conn.with_mut(|conn| {
            conn.execute(
                "CREATE TABLE IF NOT EXISTS _meta (
                    key   TEXT PRIMARY KEY,
                    value TEXT NOT NULL
                )",
                [],
            )?;
            let current: u32 = conn
                .query_row(
                    "SELECT value FROM _meta WHERE key = 'schema_version'",
                    [],
                    |r| r.get::<_, String>(0),
                )
                .optional()?
                .map_or(0, |s| s.parse().unwrap_or(0));

            for (version, sql) in MIGRATIONS {
                if *version > current {
                    let tx = conn.transaction()?;
                    tx.execute_batch(sql)?;
                    tx.execute(
                        "INSERT OR REPLACE INTO _meta (key, value) VALUES ('schema_version', ?1)",
                        params![version.to_string()],
                    )?;
                    tx.commit()?;
                }
            }
            Ok(())
        })
    }

    pub fn save_table(&self, schema: &TableSchema) -> Result<()> {
        let analyzer = schema.analyzer_json.clone();
        let fts = serde_json::to_string(&schema.fts_fields)?;
        let vectors = serde_json::to_string(&schema.vector_fields)?;
        let columns = schema.columns_json.clone();
        self.conn.with(|c| {
            c.execute(
                "INSERT OR REPLACE INTO _tables
                    (name, analyzer, fts_fields, vector_fields, columns)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![schema.name, analyzer, fts, vectors, columns],
            )?;
            Ok(())
        })
    }

    pub fn load_tables(&self) -> Result<Vec<TableSchema>> {
        self.conn.with(|c| {
            let mut stmt = c.prepare(
                "SELECT name, analyzer, fts_fields, vector_fields, columns
                   FROM _tables ORDER BY name",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, Option<String>>(4)?,
                ))
            })?;
            let mut out = Vec::new();
            for row in rows {
                let (name, analyzer_json, fts_str, vec_str, cols_opt) = row?;
                let fts_fields: Vec<String> = serde_json::from_str(&fts_str)?;
                let vector_fields: Vec<VectorFieldSchema> = serde_json::from_str(&vec_str)?;
                out.push(TableSchema {
                    name,
                    analyzer_json,
                    fts_fields,
                    vector_fields,
                    columns_json: cols_opt.unwrap_or_default(),
                });
            }
            Ok(out)
        })
    }

    pub fn drop_table(&self, name: &str) -> Result<()> {
        self.conn.with(|c| {
            c.execute("DELETE FROM _tables WHERE name = ?1", params![name])?;
            Ok(())
        })
    }

    /// Wipe the rows owned by `table` from the per-table data tables
    /// (`_documents`, `_postings`, `_doc_lengths`, `_field_stats`,
    /// `_vectors`). Run after [`Catalog::drop_table`] when the engine
    /// drops the table from its in-memory registry as well.
    pub fn purge_table_data(&self, name: &str) -> Result<()> {
        self.conn.with(|c| {
            c.execute(
                "DELETE FROM _documents WHERE table_name = ?1",
                params![name],
            )?;
            c.execute("DELETE FROM _postings WHERE table_name = ?1", params![name])?;
            c.execute(
                "DELETE FROM _doc_lengths WHERE table_name = ?1",
                params![name],
            )?;
            c.execute(
                "DELETE FROM _field_stats WHERE table_name = ?1",
                params![name],
            )?;
            c.execute("DELETE FROM _vectors WHERE table_name = ?1", params![name])?;
            Ok(())
        })
    }

    pub fn save_model(&self, name: &str, json: &str) -> Result<()> {
        self.conn.with(|c| {
            c.execute(
                "INSERT OR REPLACE INTO _models (name, body) VALUES (?1, ?2)",
                params![name, json],
            )?;
            Ok(())
        })
    }

    pub fn load_models(&self) -> Result<Vec<(String, String)>> {
        self.conn.with(|c| {
            let mut stmt = c.prepare("SELECT name, body FROM _models ORDER BY name")?;
            let rows =
                stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    pub fn load_model(&self, name: &str) -> Result<Option<String>> {
        self.conn.with(|c| {
            Ok(c.query_row(
                "SELECT body FROM _models WHERE name = ?1",
                params![name],
                |r| r.get::<_, String>(0),
            )
            .optional()?)
        })
    }

    pub fn drop_model(&self, name: &str) -> Result<()> {
        self.conn.with(|c| {
            c.execute("DELETE FROM _models WHERE name = ?1", params![name])?;
            Ok(())
        })
    }
}

/// Migrations applied in order. Each `(version, sql)` is run in a single
/// transaction; the `_meta.schema_version` row is bumped on success.
const MIGRATIONS: &[(u32, &str)] = &[
    (
        1,
        r"
    CREATE TABLE IF NOT EXISTS _tables (
        name           TEXT PRIMARY KEY,
        analyzer       TEXT NOT NULL,
        fts_fields     TEXT NOT NULL,
        vector_fields  TEXT NOT NULL
    );

    CREATE TABLE IF NOT EXISTS _documents (
        table_name TEXT NOT NULL,
        doc_id     INTEGER NOT NULL,
        body       TEXT NOT NULL,
        PRIMARY KEY (table_name, doc_id)
    );

    CREATE TABLE IF NOT EXISTS _postings (
        table_name TEXT NOT NULL,
        field      TEXT NOT NULL,
        term       TEXT NOT NULL,
        doc_id     INTEGER NOT NULL,
        positions  BLOB NOT NULL,
        PRIMARY KEY (table_name, field, term, doc_id)
    );
    CREATE INDEX IF NOT EXISTS _postings_term_idx
        ON _postings (table_name, field, term);
    CREATE INDEX IF NOT EXISTS _postings_doc_idx
        ON _postings (table_name, doc_id);

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

    CREATE TABLE IF NOT EXISTS _vectors (
        table_name TEXT NOT NULL,
        field      TEXT NOT NULL,
        doc_id     INTEGER NOT NULL,
        vector     BLOB NOT NULL,
        PRIMARY KEY (table_name, field, doc_id)
    );
    ",
    ),
    (
        2,
        r"
    CREATE TABLE IF NOT EXISTS _models (
        name TEXT PRIMARY KEY,
        body TEXT NOT NULL
    );
    ",
    ),
    (
        3,
        r"
    ALTER TABLE _tables ADD COLUMN columns TEXT;
    ",
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> Catalog {
        let mc = ManagedConnection::open_in_memory().unwrap();
        Catalog::open(mc).unwrap()
    }

    #[test]
    fn migration_creates_tables_table() {
        let cat = fresh();
        cat.conn
            .with(|c| {
                let count: u32 = c.query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE name = '_tables'",
                    [],
                    |r| r.get(0),
                )?;
                assert_eq!(count, 1);
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn save_load_round_trip() {
        let cat = fresh();
        let schema = TableSchema {
            name: "articles".into(),
            analyzer_json:
                "{\"tokenizer\":{\"type\":\"standard\"},\"token_filters\":[],\"char_filters\":[]}"
                    .into(),
            fts_fields: vec!["title".into(), "body".into()],
            vector_fields: vec![VectorFieldSchema {
                field: "embedding".into(),
                dimensions: 768,
            }],
            columns_json: String::new(),
        };
        cat.save_table(&schema).unwrap();
        let loaded = cat.load_tables().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name, "articles");
        assert_eq!(loaded[0].fts_fields, vec!["title", "body"]);
        assert_eq!(loaded[0].vector_fields.len(), 1);
        assert_eq!(loaded[0].vector_fields[0].field, "embedding");
        assert_eq!(loaded[0].vector_fields[0].dimensions, 768);
        assert!(loaded[0].columns_json.is_empty());
    }

    #[test]
    fn migration_is_idempotent() {
        let mc = ManagedConnection::open_in_memory().unwrap();
        let _cat1 = Catalog::open(mc.clone()).unwrap();
        // Reopen on the same handle: should not re-run migrations or
        // raise an error.
        let _cat2 = Catalog::open(mc).unwrap();
    }
}
