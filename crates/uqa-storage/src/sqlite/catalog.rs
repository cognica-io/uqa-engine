//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog: schema versioning, migrations, and persisted table metadata.
//!
//! The catalog owns a single `_metadata` table that records the schema version
//! plus a `_tables` table holding per-table analyzer config and the lists
//! of FTS / vector fields registered on each table. Concrete data tables
//! (`_documents`, `_postings`, `_vectors`, ...) are created by their
//! respective stores.

use rusqlite::{params, OptionalExtension};

use crate::backend::{StorageBackendError, StorageBackendResult};
use crate::catalog::{
    CatalogFacade, CatalogIndexRow, ColumnStatsInput, ColumnStatsRow, EdgeRow, ForeignTableRow,
    TableSchema, VectorFieldSchema,
};
use crate::sqlite::connection::{ManagedConnection, Result};

use super::catalog_lifecycle::{
    columns_json_references, delete_table_rows_if_exists, drop_fts_aux_tables_for_field,
    drop_fts_aux_tables_for_table, quote_sql_identifier, rename_field_rows_or_keep_existing,
    rename_fts_aux_tables_for_field, renamed_columns_json, table_exists,
    update_table_name_rows_if_exists,
};

/// Bump this every time a migration is added.
pub const CURRENT_SCHEMA_VERSION: u32 = 12;

pub struct Catalog {
    conn: ManagedConnection,
    fts_storage_was_reset: bool,
}

impl Catalog {
    /// Open (or create) the catalog and run any pending migrations.
    pub fn open(conn: ManagedConnection) -> Result<Self> {
        let mut cat = Self {
            conn,
            fts_storage_was_reset: false,
        };
        cat.fts_storage_was_reset = cat.run_migrations()?;
        Ok(cat)
    }

    pub fn connection(&self) -> ManagedConnection {
        self.conn.clone()
    }

    fn run_migrations(&self) -> Result<bool> {
        self.conn.with_mut(|conn| {
            // Older catalogs (pre-v7) used the table name `_meta`. v7
            // renames it to `_metadata`; promote the legacy table before
            // any migration query touches it.
            let legacy_meta_only: bool = conn
                .query_row(
                    "SELECT \
                        (SELECT COUNT(*) FROM sqlite_master \
                          WHERE type='table' AND name='_meta') > 0 \
                     AND (SELECT COUNT(*) FROM sqlite_master \
                            WHERE type='table' AND name='_metadata') = 0",
                    [],
                    |r| r.get::<_, i64>(0),
                )
                .optional()?
                .is_some_and(|n| n != 0);
            if legacy_meta_only {
                conn.execute("ALTER TABLE _meta RENAME TO _metadata", [])?;
            }
            conn.execute(
                "CREATE TABLE IF NOT EXISTS _metadata (
                    key   TEXT PRIMARY KEY,
                    value TEXT NOT NULL
                )",
                [],
            )?;
            let current: u32 = conn
                .query_row(
                    "SELECT value FROM _metadata WHERE key = 'schema_version'",
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
                        "INSERT OR REPLACE INTO _metadata (key, value) \
                         VALUES ('schema_version', ?1)",
                        params![version.to_string()],
                    )?;
                    tx.commit()?;
                }
            }
            Self::ensure_column_stats_shape(conn)?;
            let fts_storage_was_reset = Self::ensure_fts_storage_shape(conn)?;
            Ok(fts_storage_was_reset)
        })
    }

    fn ensure_fts_storage_shape(conn: &rusqlite::Connection) -> Result<bool> {
        let doc_lengths = Self::table_columns(conn, "_doc_lengths")?;
        let postings = Self::table_columns(conn, "_postings")?;
        let doc_lengths_ok = doc_lengths
            .as_ref()
            .is_some_and(|cols| cols.contains_key("field") && cols.contains_key("length"));
        let postings_ok = postings.as_ref().is_some_and(|cols| {
            cols.get("positions")
                .is_some_and(|ty| ty.eq_ignore_ascii_case("BLOB"))
        });
        if doc_lengths_ok && postings_ok {
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
            DROP TABLE IF EXISTS _doc_lengths;
            DROP TABLE IF EXISTS _field_stats;

            CREATE TABLE IF NOT EXISTS _postings (
                table_name TEXT NOT NULL,
                field      TEXT NOT NULL,
                term       TEXT NOT NULL,
                doc_id     INTEGER NOT NULL,
                positions  BLOB NOT NULL,
                PRIMARY KEY (table_name, field, term, doc_id)
            );
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
            ",
        )?;
        Ok(true)
    }

    fn table_columns(
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

    fn ensure_column_stats_shape(conn: &rusqlite::Connection) -> Result<()> {
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

    /// Store an arbitrary key/value pair in the `_metadata` table.
    /// Mirrors the canonical UQA implementation's `Catalog.set_metadata`.
    pub fn set_metadata(&self, key: &str, value: &str) -> Result<()> {
        self.conn.with(|c| {
            c.execute(
                "INSERT OR REPLACE INTO _metadata (key, value) VALUES (?1, ?2)",
                params![key, value],
            )?;
            Ok(())
        })
    }

    /// Read a key/value pair from the `_metadata` table.
    pub fn get_metadata(&self, key: &str) -> Result<Option<String>> {
        self.conn.with(|c| {
            let v: Option<String> = c
                .query_row(
                    "SELECT value FROM _metadata WHERE key = ?1",
                    params![key],
                    |r| r.get(0),
                )
                .optional()?;
            Ok(v)
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
    /// `_vectors`, IVF metadata). Run after [`Catalog::drop_table`]
    /// when the engine drops the table from its in-memory registry as
    /// well.
    pub fn purge_table_data(&self, name: &str) -> Result<()> {
        self.conn.with(|c| {
            for table in [
                "_documents",
                "_document_blobs",
                "_postings",
                "_doc_lengths",
                "_field_stats",
                "_vectors",
                "_ivf_indexes",
                "_ivf_centroids",
                "_ivf_assignments",
                "_column_stats",
                "_btree_indexes",
            ] {
                delete_table_rows_if_exists(c, table, name)?;
            }
            drop_fts_aux_tables_for_table(c, name)?;
            Ok(())
        })
    }

    pub fn rename_table_data(&self, from: &str, to: &str) -> Result<()> {
        self.conn.with(|c| {
            c.execute(
                "UPDATE _tables SET name = ?2 WHERE name = ?1",
                params![from, to],
            )?;
            for table in [
                "_documents",
                "_document_blobs",
                "_postings",
                "_doc_lengths",
                "_field_stats",
                "_vectors",
                "_ivf_indexes",
                "_ivf_centroids",
                "_ivf_assignments",
                "_column_stats",
                "_table_field_analyzers",
                "_catalog_indexes",
                "_btree_indexes",
            ] {
                update_table_name_rows_if_exists(c, table, from, to)?;
            }
            drop_fts_aux_tables_for_table(c, from)?;
            Ok(())
        })
    }

    pub fn drop_column_data(&self, table_name: &str, column_name: &str) -> Result<()> {
        let indexes = self.catalog_indexes_referencing_column(table_name, column_name)?;
        self.conn.with(|c| {
            if table_exists(c, "_document_blobs")? {
                c.execute(
                    "DELETE FROM _document_blobs WHERE table_name = ?1 AND field_name = ?2",
                    params![table_name, column_name],
                )?;
            }
            for table in [
                "_postings",
                "_doc_lengths",
                "_field_stats",
                "_vectors",
                "_ivf_indexes",
                "_ivf_centroids",
                "_ivf_assignments",
                "_btree_indexes",
            ] {
                c.execute(
                    &format!("DELETE FROM {table} WHERE table_name = ?1 AND field = ?2"),
                    params![table_name, column_name],
                )?;
            }
            c.execute(
                "DELETE FROM _column_stats WHERE table_name = ?1 AND column_name = ?2",
                params![table_name, column_name],
            )?;
            c.execute(
                "DELETE FROM _table_field_analyzers WHERE table_name = ?1 AND field = ?2",
                params![table_name, column_name],
            )?;
            for index_name in indexes {
                c.execute(
                    "DELETE FROM _catalog_indexes WHERE name = ?1",
                    params![index_name],
                )?;
            }
            drop_fts_aux_tables_for_field(c, table_name, column_name)?;
            Ok(())
        })
    }

    pub fn rename_column_data(&self, table_name: &str, from: &str, to: &str) -> Result<()> {
        let index_updates = self.catalog_index_column_renames(table_name, from, to)?;
        self.conn.with(|c| {
            rename_field_rows_or_keep_existing(
                c,
                "_document_blobs",
                "field_name",
                table_name,
                from,
                to,
            )?;
            for table in [
                "_postings",
                "_doc_lengths",
                "_field_stats",
                "_vectors",
                "_ivf_indexes",
                "_ivf_centroids",
                "_ivf_assignments",
                "_btree_indexes",
            ] {
                rename_field_rows_or_keep_existing(c, table, "field", table_name, from, to)?;
            }
            rename_field_rows_or_keep_existing(
                c,
                "_column_stats",
                "column_name",
                table_name,
                from,
                to,
            )?;
            rename_field_rows_or_keep_existing(
                c,
                "_table_field_analyzers",
                "field",
                table_name,
                from,
                to,
            )?;
            for (index_name, columns_json) in index_updates {
                c.execute(
                    "UPDATE _catalog_indexes
                        SET columns = ?2
                      WHERE name = ?1",
                    params![index_name, columns_json],
                )?;
            }
            rename_fts_aux_tables_for_field(c, table_name, from, to)?;
            Ok(())
        })
    }

    fn catalog_indexes_referencing_column(
        &self,
        table_name: &str,
        column_name: &str,
    ) -> Result<Vec<String>> {
        let mut out = Vec::new();
        for row in self.load_catalog_indexes()? {
            if row.table_name == table_name
                && columns_json_references(&row.columns_json, column_name)
            {
                out.push(row.name);
            }
        }
        Ok(out)
    }

    fn catalog_index_column_renames(
        &self,
        table_name: &str,
        from: &str,
        to: &str,
    ) -> Result<Vec<(String, String)>> {
        let mut out = Vec::new();
        for row in self.load_catalog_indexes()? {
            if row.table_name != table_name {
                continue;
            }
            if let Some(columns_json) = renamed_columns_json(&row.columns_json, from, to) {
                out.push((row.name, columns_json));
            }
        }
        Ok(out)
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

    /// Persist Bayesian calibration parameters for a named signal.
    /// Matches UQA behavior for `Catalog.save_scoring_params`.
    pub fn save_scoring_params(&self, name: &str, params_json: &str) -> Result<()> {
        self.conn.with(|c| {
            c.execute(
                "INSERT OR REPLACE INTO _scoring_params (name, params) VALUES (?1, ?2)",
                params![name, params_json],
            )?;
            Ok(())
        })
    }

    /// Load persisted scoring parameters for a single signal.
    pub fn load_scoring_params(&self, name: &str) -> Result<Option<String>> {
        self.conn.with(|c| {
            Ok(c.query_row(
                "SELECT params FROM _scoring_params WHERE name = ?1",
                params![name],
                |r| r.get::<_, String>(0),
            )
            .optional()?)
        })
    }

    /// Load every persisted `(name, params_json)` pair sorted by name.
    pub fn load_all_scoring_params(&self) -> Result<Vec<(String, String)>> {
        self.conn.with(|c| {
            let mut stmt = c.prepare("SELECT name, params FROM _scoring_params ORDER BY name")?;
            let rows =
                stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    /// Delete persisted scoring parameters for a single signal.
    pub fn drop_scoring_params(&self, name: &str) -> Result<()> {
        self.conn.with(|c| {
            c.execute("DELETE FROM _scoring_params WHERE name = ?1", params![name])?;
            Ok(())
        })
    }

    /// Register the existence of a named graph in the catalog.
    /// Matches UQA behavior for `Catalog.save_named_graph`.
    pub fn save_named_graph(&self, name: &str) -> Result<()> {
        self.conn.with(|c| {
            c.execute(
                "INSERT OR IGNORE INTO _named_graphs (name) VALUES (?1)",
                params![name],
            )?;
            Ok(())
        })
    }

    /// Drop the named-graph registry row plus every membership entry
    /// that scopes a vertex or edge to this graph. Vertex / edge rows
    /// stay in `_graph_vertices` / `_graph_edges` until they go
    /// orphan; call [`Catalog::purge_orphan_graph_entities`] after to
    /// GC them. Matches UQA behavior for `Catalog.drop_named_graph` plus the
    /// orphan sweep that the engine performs on its behalf.
    pub fn drop_named_graph(&self, name: &str) -> Result<()> {
        self.conn.with(|c| {
            c.execute("DELETE FROM _named_graphs WHERE name = ?1", params![name])?;
            c.execute(
                "DELETE FROM _graph_membership WHERE graph_name = ?1",
                params![name],
            )?;
            Ok(())
        })
    }

    /// Sorted list of every persisted named graph.
    /// Matches UQA behavior for `Catalog.load_named_graphs`.
    pub fn load_named_graphs(&self) -> Result<Vec<String>> {
        self.conn.with(|c| {
            let mut stmt = c.prepare("SELECT name FROM _named_graphs ORDER BY name")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    /// Persist a vertex by global id. `properties_json` is the JSON
    /// encoding of the property map. Matches UQA behavior for
    /// `Catalog.save_vertex` extended with the `label` column the
    /// `SQLiteGraphStore` writes alongside it.
    pub fn save_vertex(&self, vertex_id: u64, label: &str, properties_json: &str) -> Result<()> {
        self.conn.with(|c| {
            c.execute(
                "INSERT OR REPLACE INTO _graph_vertices (vertex_id, label, properties_json) \
                 VALUES (?1, ?2, ?3)",
                params![vertex_id as i64, label, properties_json],
            )?;
            Ok(())
        })
    }

    /// Delete a vertex by global id.
    pub fn delete_vertex(&self, vertex_id: u64) -> Result<()> {
        self.conn.with(|c| {
            c.execute(
                "DELETE FROM _graph_vertices WHERE vertex_id = ?1",
                params![vertex_id as i64],
            )?;
            Ok(())
        })
    }

    /// Every vertex row sorted by id, returned as
    /// `(vertex_id, label, properties_json)` so the caller rebuilds
    /// the `Vertex` from the typed columns plus the JSON-encoded
    /// property map. Matches UQA behavior for `Catalog.load_vertices`.
    pub fn load_vertices(&self) -> Result<Vec<(u64, String, String)>> {
        self.conn.with(|c| {
            let mut stmt = c.prepare(
                "SELECT vertex_id, label, properties_json FROM _graph_vertices ORDER BY vertex_id",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?;
            let mut out = Vec::new();
            for row in rows {
                let (id, label, props) = row?;
                out.push((id as u64, label, props));
            }
            Ok(out)
        })
    }

    /// Persist an edge by global id with its source / target vertices,
    /// label, and JSON-encoded property map. Matches UQA behavior for
    /// `Catalog.save_edge`.
    pub fn save_edge(
        &self,
        edge_id: u64,
        source_id: u64,
        target_id: u64,
        label: &str,
        properties_json: &str,
    ) -> Result<()> {
        self.conn.with(|c| {
            c.execute(
                "INSERT OR REPLACE INTO _graph_edges \
                    (edge_id, source_id, target_id, label, properties_json) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    edge_id as i64,
                    source_id as i64,
                    target_id as i64,
                    label,
                    properties_json
                ],
            )?;
            Ok(())
        })
    }

    /// Delete an edge by global id.
    pub fn delete_edge(&self, edge_id: u64) -> Result<()> {
        self.conn.with(|c| {
            c.execute(
                "DELETE FROM _graph_edges WHERE edge_id = ?1",
                params![edge_id as i64],
            )?;
            Ok(())
        })
    }

    /// Every edge row sorted by id. Matches UQA behavior for `Catalog.load_edges`.
    pub fn load_edges(&self) -> Result<Vec<EdgeRow>> {
        self.conn.with(|c| {
            let mut stmt = c.prepare(
                "SELECT edge_id, source_id, target_id, label, properties_json \
                   FROM _graph_edges ORDER BY edge_id",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                ))
            })?;
            let mut out = Vec::new();
            for row in rows {
                let (id, src, tgt, label, props) = row?;
                out.push(EdgeRow {
                    edge_id: id as u64,
                    source_id: src as u64,
                    target_id: tgt as u64,
                    label,
                    properties_json: props,
                });
            }
            Ok(out)
        })
    }

    /// Attach `entity_id` (a vertex when `entity_type == "vertex"`, an
    /// edge when `"edge"`) to `graph_name`. The same entity can sit in
    /// many graphs; the row is keyed by the full triple so duplicate
    /// attaches no-op.
    pub fn save_graph_membership(
        &self,
        entity_type: &str,
        entity_id: u64,
        graph_name: &str,
    ) -> Result<()> {
        self.conn.with(|c| {
            c.execute(
                "INSERT OR IGNORE INTO _graph_membership \
                    (entity_type, entity_id, graph_name) \
                 VALUES (?1, ?2, ?3)",
                params![entity_type, entity_id as i64, graph_name],
            )?;
            Ok(())
        })
    }

    /// Detach `entity_id` from `graph_name`.
    pub fn delete_graph_membership(
        &self,
        entity_type: &str,
        entity_id: u64,
        graph_name: &str,
    ) -> Result<()> {
        self.conn.with(|c| {
            c.execute(
                "DELETE FROM _graph_membership \
                  WHERE entity_type = ?1 AND entity_id = ?2 AND graph_name = ?3",
                params![entity_type, entity_id as i64, graph_name],
            )?;
            Ok(())
        })
    }

    /// Detach every entity from `graph_name`. Used as the prelude to a
    /// full graph drop / Cypher resync.
    pub fn delete_graph_membership_for_graph(&self, graph_name: &str) -> Result<()> {
        self.conn.with(|c| {
            c.execute(
                "DELETE FROM _graph_membership WHERE graph_name = ?1",
                params![graph_name],
            )?;
            Ok(())
        })
    }

    /// Every membership row, returned as `(entity_type, entity_id, graph_name)`.
    pub fn load_graph_memberships(&self) -> Result<Vec<(String, u64, String)>> {
        self.conn.with(|c| {
            let mut stmt = c.prepare(
                "SELECT entity_type, entity_id, graph_name FROM _graph_membership \
                  ORDER BY graph_name, entity_type, entity_id",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?;
            let mut out = Vec::new();
            for row in rows {
                let (ty, id, graph) = row?;
                out.push((ty, id as u64, graph));
            }
            Ok(out)
        })
    }

    /// Drop vertex / edge rows that no membership row still references.
    /// Run after a detach / drop to garbage-collect orphaned entities.
    pub fn purge_orphan_graph_entities(&self) -> Result<()> {
        self.conn.with(|c| {
            c.execute(
                "DELETE FROM _graph_vertices \
                  WHERE vertex_id NOT IN ( \
                    SELECT entity_id FROM _graph_membership WHERE entity_type = 'vertex' \
                  )",
                [],
            )?;
            c.execute(
                "DELETE FROM _graph_edges \
                  WHERE edge_id NOT IN ( \
                    SELECT entity_id FROM _graph_membership WHERE entity_type = 'edge' \
                  )",
                [],
            )?;
            Ok(())
        })
    }

    // -- Named analyzers ---------------------------------------------------

    /// Persist a named analyzer configuration. Matches UQA behavior for
    /// `Catalog.save_analyzer`.
    pub fn save_analyzer(&self, name: &str, config_json: &str) -> Result<()> {
        self.conn.with(|c| {
            c.execute(
                "INSERT OR REPLACE INTO _analyzers (name, config_json) VALUES (?1, ?2)",
                params![name, config_json],
            )?;
            Ok(())
        })
    }

    pub fn drop_analyzer(&self, name: &str) -> Result<()> {
        self.conn.with(|c| {
            c.execute("DELETE FROM _analyzers WHERE name = ?1", params![name])?;
            Ok(())
        })
    }

    pub fn load_analyzers(&self) -> Result<Vec<(String, String)>> {
        self.conn.with(|c| {
            let mut stmt = c.prepare("SELECT name, config_json FROM _analyzers ORDER BY name")?;
            let rows =
                stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    // -- Per-field analyzer overrides --------------------------------------

    /// Persist a `(table, field, phase) -> analyzer_name` row. Mirrors
    /// Persist a table-field analyzer mapping.
    pub fn save_table_field_analyzer(
        &self,
        table_name: &str,
        field: &str,
        phase: &str,
        analyzer_name: &str,
    ) -> Result<()> {
        self.conn.with(|c| {
            c.execute(
                "INSERT OR REPLACE INTO _table_field_analyzers \
                    (table_name, field, phase, analyzer_name) \
                 VALUES (?1, ?2, ?3, ?4)",
                params![table_name, field, phase, analyzer_name],
            )?;
            Ok(())
        })
    }

    pub fn drop_table_field_analyzers(&self, table_name: &str) -> Result<()> {
        self.conn.with(|c| {
            c.execute(
                "DELETE FROM _table_field_analyzers WHERE table_name = ?1",
                params![table_name],
            )?;
            Ok(())
        })
    }

    /// Every `(table_name, field, phase, analyzer_name)` row sorted by
    /// `(table_name, field, phase)`.
    pub fn load_table_field_analyzers(&self) -> Result<Vec<(String, String, String, String)>> {
        self.conn.with(|c| {
            let mut stmt = c.prepare(
                "SELECT table_name, field, phase, analyzer_name FROM _table_field_analyzers \
                  ORDER BY table_name, field, phase",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    // -- Foreign servers ---------------------------------------------------

    pub fn save_foreign_server(
        &self,
        name: &str,
        fdw_type: &str,
        options_json: &str,
    ) -> Result<()> {
        self.conn.with(|c| {
            c.execute(
                "INSERT OR REPLACE INTO _foreign_servers (name, fdw_type, options) \
                 VALUES (?1, ?2, ?3)",
                params![name, fdw_type, options_json],
            )?;
            Ok(())
        })
    }

    pub fn drop_foreign_server(&self, name: &str) -> Result<()> {
        self.conn.with(|c| {
            c.execute(
                "DELETE FROM _foreign_servers WHERE name = ?1",
                params![name],
            )?;
            Ok(())
        })
    }

    pub fn load_foreign_servers(&self) -> Result<Vec<(String, String, String)>> {
        self.conn.with(|c| {
            let mut stmt =
                c.prepare("SELECT name, fdw_type, options FROM _foreign_servers ORDER BY name")?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    // -- Foreign tables ----------------------------------------------------

    pub fn save_foreign_table(
        &self,
        name: &str,
        server_name: &str,
        columns_json: &str,
        options_json: &str,
    ) -> Result<()> {
        self.conn.with(|c| {
            c.execute(
                "INSERT OR REPLACE INTO _foreign_tables \
                    (name, server_name, columns_json, options) \
                 VALUES (?1, ?2, ?3, ?4)",
                params![name, server_name, columns_json, options_json],
            )?;
            Ok(())
        })
    }

    pub fn drop_foreign_table(&self, name: &str) -> Result<()> {
        self.conn.with(|c| {
            c.execute("DELETE FROM _foreign_tables WHERE name = ?1", params![name])?;
            Ok(())
        })
    }

    pub fn load_foreign_tables(&self) -> Result<Vec<ForeignTableRow>> {
        self.conn.with(|c| {
            let mut stmt = c.prepare(
                "SELECT name, server_name, columns_json, options FROM _foreign_tables \
                  ORDER BY name",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })?;
            let mut out = Vec::new();
            for row in rows {
                let (name, server, cols, opts) = row?;
                out.push(ForeignTableRow {
                    name,
                    server_name: server,
                    columns_json: cols,
                    options_json: opts,
                });
            }
            Ok(out)
        })
    }

    // -- Catalog indexes (CREATE INDEX state) ------------------------------

    pub fn save_catalog_index(
        &self,
        name: &str,
        index_type: &str,
        table_name: &str,
        columns_json: &str,
        parameters_json: &str,
    ) -> Result<()> {
        self.conn.with(|c| {
            c.execute(
                "INSERT OR REPLACE INTO _catalog_indexes \
                    (name, index_type, table_name, columns, parameters) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![name, index_type, table_name, columns_json, parameters_json],
            )?;
            Ok(())
        })
    }

    pub fn drop_catalog_index(&self, name: &str) -> Result<()> {
        self.conn.with(|c| {
            c.execute(
                "DELETE FROM _catalog_indexes WHERE name = ?1",
                params![name],
            )?;
            Ok(())
        })
    }

    pub fn drop_catalog_indexes_for_table(&self, table_name: &str) -> Result<()> {
        self.conn.with(|c| {
            c.execute(
                "DELETE FROM _catalog_indexes WHERE table_name = ?1",
                params![table_name],
            )?;
            Ok(())
        })
    }

    pub fn load_catalog_indexes(&self) -> Result<Vec<CatalogIndexRow>> {
        self.conn.with(|c| {
            let mut stmt = c.prepare(
                "SELECT name, index_type, table_name, columns, parameters \
                   FROM _catalog_indexes ORDER BY name",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                ))
            })?;
            let mut out = Vec::new();
            for row in rows {
                let (name, ty, table, cols, params_json) = row?;
                out.push(CatalogIndexRow {
                    name,
                    index_type: ty,
                    table_name: table,
                    columns_json: cols,
                    parameters_json: params_json,
                });
            }
            Ok(out)
        })
    }

    // -- Path indexes ------------------------------------------------------

    pub fn save_path_index(&self, graph_name: &str, label_sequences_json: &str) -> Result<()> {
        self.conn.with(|c| {
            c.execute(
                "INSERT OR REPLACE INTO _path_indexes (graph_name, label_sequences) \
                 VALUES (?1, ?2)",
                params![graph_name, label_sequences_json],
            )?;
            Ok(())
        })
    }

    pub fn drop_path_index(&self, graph_name: &str) -> Result<()> {
        self.conn.with(|c| {
            c.execute(
                "DELETE FROM _path_indexes WHERE graph_name = ?1",
                params![graph_name],
            )?;
            Ok(())
        })
    }

    /// `(graph_name, label_sequences_json)` for every persisted path index.
    pub fn load_path_indexes(&self) -> Result<Vec<(String, String)>> {
        self.conn.with(|c| {
            let mut stmt = c.prepare(
                "SELECT graph_name, label_sequences FROM _path_indexes ORDER BY graph_name",
            )?;
            let rows =
                stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    /// Persist a per-column ANALYZE summary so the planner still has
    /// cardinality / range estimates after a restart. `min_value` and
    /// `max_value` are stored as strings (JSON when the value isn't
    /// natively textual) so the column type is irrelevant.
    pub fn save_column_stats(&self, stats: ColumnStatsInput<'_>) -> Result<()> {
        self.conn.with(|c| {
            c.execute(
                "INSERT OR REPLACE INTO _column_stats
                    (table_name, column_name, distinct_count, null_count,
                     min_value, max_value, row_count,
                     histogram, mcv_values, mcv_frequencies)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    stats.table_name,
                    stats.column_name,
                    stats.distinct_count,
                    stats.null_count,
                    stats.min_value,
                    stats.max_value,
                    stats.row_count,
                    stats.histogram_json,
                    stats.mcv_values_json,
                    stats.mcv_frequencies_json,
                ],
            )?;
            Ok(())
        })
    }

    pub fn load_column_stats(&self, table_name: &str) -> Result<Vec<ColumnStatsRow>> {
        self.conn.with(|c| {
            let mut stmt = c.prepare(
                "SELECT column_name, distinct_count, null_count,
                        min_value, max_value, row_count,
                        histogram, mcv_values, mcv_frequencies
                   FROM _column_stats
                  WHERE table_name = ?1
                  ORDER BY column_name",
            )?;
            let rows = stmt.query_map(params![table_name], |r| {
                Ok(ColumnStatsRow {
                    column_name: r.get::<_, String>(0)?,
                    distinct_count: r.get::<_, i64>(1)?,
                    null_count: r.get::<_, i64>(2)?,
                    min_value: r.get::<_, Option<String>>(3)?,
                    max_value: r.get::<_, Option<String>>(4)?,
                    row_count: r.get::<_, i64>(5)?,
                    histogram_json: r.get::<_, String>(6)?,
                    mcv_values_json: r.get::<_, String>(7)?,
                    mcv_frequencies_json: r.get::<_, String>(8)?,
                })
            })?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    pub fn delete_column_stats(&self, table_name: &str) -> Result<()> {
        self.conn.with(|c| {
            c.execute(
                "DELETE FROM _column_stats WHERE table_name = ?1",
                params![table_name],
            )?;
            Ok(())
        })
    }
}

fn into_storage_result<T>(result: Result<T>) -> StorageBackendResult<T> {
    result.map_err(StorageBackendError::from)
}

impl CatalogFacade for Catalog {
    fn set_metadata(&self, key: &str, value: &str) -> StorageBackendResult<()> {
        into_storage_result(Catalog::set_metadata(self, key, value))
    }

    fn get_metadata(&self, key: &str) -> StorageBackendResult<Option<String>> {
        into_storage_result(Catalog::get_metadata(self, key))
    }

    fn fts_storage_was_reset(&self) -> bool {
        self.fts_storage_was_reset
    }

    fn save_table(&self, schema: &TableSchema) -> StorageBackendResult<()> {
        into_storage_result(Catalog::save_table(self, schema))
    }

    fn load_tables(&self) -> StorageBackendResult<Vec<TableSchema>> {
        into_storage_result(Catalog::load_tables(self))
    }

    fn drop_table(&self, name: &str) -> StorageBackendResult<()> {
        into_storage_result(Catalog::drop_table(self, name))
    }

    fn purge_table_data(&self, name: &str) -> StorageBackendResult<()> {
        into_storage_result(Catalog::purge_table_data(self, name))
    }

    fn rename_table_data(&self, from: &str, to: &str) -> StorageBackendResult<()> {
        into_storage_result(Catalog::rename_table_data(self, from, to))
    }

    fn drop_column_data(&self, table_name: &str, column_name: &str) -> StorageBackendResult<()> {
        into_storage_result(Catalog::drop_column_data(self, table_name, column_name))
    }

    fn rename_column_data(
        &self,
        table_name: &str,
        from: &str,
        to: &str,
    ) -> StorageBackendResult<()> {
        into_storage_result(Catalog::rename_column_data(self, table_name, from, to))
    }

    fn save_model(&self, name: &str, json: &str) -> StorageBackendResult<()> {
        into_storage_result(Catalog::save_model(self, name, json))
    }

    fn load_models(&self) -> StorageBackendResult<Vec<(String, String)>> {
        into_storage_result(Catalog::load_models(self))
    }

    fn load_model(&self, name: &str) -> StorageBackendResult<Option<String>> {
        into_storage_result(Catalog::load_model(self, name))
    }

    fn drop_model(&self, name: &str) -> StorageBackendResult<()> {
        into_storage_result(Catalog::drop_model(self, name))
    }

    fn save_scoring_params(&self, name: &str, params_json: &str) -> StorageBackendResult<()> {
        into_storage_result(Catalog::save_scoring_params(self, name, params_json))
    }

    fn load_scoring_params(&self, name: &str) -> StorageBackendResult<Option<String>> {
        into_storage_result(Catalog::load_scoring_params(self, name))
    }

    fn load_all_scoring_params(&self) -> StorageBackendResult<Vec<(String, String)>> {
        into_storage_result(Catalog::load_all_scoring_params(self))
    }

    fn drop_scoring_params(&self, name: &str) -> StorageBackendResult<()> {
        into_storage_result(Catalog::drop_scoring_params(self, name))
    }

    fn save_named_graph(&self, name: &str) -> StorageBackendResult<()> {
        into_storage_result(Catalog::save_named_graph(self, name))
    }

    fn drop_named_graph(&self, name: &str) -> StorageBackendResult<()> {
        into_storage_result(Catalog::drop_named_graph(self, name))
    }

    fn load_named_graphs(&self) -> StorageBackendResult<Vec<String>> {
        into_storage_result(Catalog::load_named_graphs(self))
    }

    fn save_vertex(
        &self,
        vertex_id: u64,
        label: &str,
        properties_json: &str,
    ) -> StorageBackendResult<()> {
        into_storage_result(Catalog::save_vertex(
            self,
            vertex_id,
            label,
            properties_json,
        ))
    }

    fn delete_vertex(&self, vertex_id: u64) -> StorageBackendResult<()> {
        into_storage_result(Catalog::delete_vertex(self, vertex_id))
    }

    fn load_vertices(&self) -> StorageBackendResult<Vec<(u64, String, String)>> {
        into_storage_result(Catalog::load_vertices(self))
    }

    fn save_edge(
        &self,
        edge_id: u64,
        source_id: u64,
        target_id: u64,
        label: &str,
        properties_json: &str,
    ) -> StorageBackendResult<()> {
        into_storage_result(Catalog::save_edge(
            self,
            edge_id,
            source_id,
            target_id,
            label,
            properties_json,
        ))
    }

    fn delete_edge(&self, edge_id: u64) -> StorageBackendResult<()> {
        into_storage_result(Catalog::delete_edge(self, edge_id))
    }

    fn load_edges(&self) -> StorageBackendResult<Vec<EdgeRow>> {
        into_storage_result(Catalog::load_edges(self))
    }

    fn save_graph_membership(
        &self,
        entity_type: &str,
        entity_id: u64,
        graph_name: &str,
    ) -> StorageBackendResult<()> {
        into_storage_result(Catalog::save_graph_membership(
            self,
            entity_type,
            entity_id,
            graph_name,
        ))
    }

    fn delete_graph_membership(
        &self,
        entity_type: &str,
        entity_id: u64,
        graph_name: &str,
    ) -> StorageBackendResult<()> {
        into_storage_result(Catalog::delete_graph_membership(
            self,
            entity_type,
            entity_id,
            graph_name,
        ))
    }

    fn delete_graph_membership_for_graph(&self, graph_name: &str) -> StorageBackendResult<()> {
        into_storage_result(Catalog::delete_graph_membership_for_graph(self, graph_name))
    }

    fn load_graph_memberships(&self) -> StorageBackendResult<Vec<(String, u64, String)>> {
        into_storage_result(Catalog::load_graph_memberships(self))
    }

    fn purge_orphan_graph_entities(&self) -> StorageBackendResult<()> {
        into_storage_result(Catalog::purge_orphan_graph_entities(self))
    }

    fn save_analyzer(&self, name: &str, config_json: &str) -> StorageBackendResult<()> {
        into_storage_result(Catalog::save_analyzer(self, name, config_json))
    }

    fn drop_analyzer(&self, name: &str) -> StorageBackendResult<()> {
        into_storage_result(Catalog::drop_analyzer(self, name))
    }

    fn load_analyzers(&self) -> StorageBackendResult<Vec<(String, String)>> {
        into_storage_result(Catalog::load_analyzers(self))
    }

    fn save_table_field_analyzer(
        &self,
        table_name: &str,
        field: &str,
        phase: &str,
        analyzer_name: &str,
    ) -> StorageBackendResult<()> {
        into_storage_result(Catalog::save_table_field_analyzer(
            self,
            table_name,
            field,
            phase,
            analyzer_name,
        ))
    }

    fn drop_table_field_analyzers(&self, table_name: &str) -> StorageBackendResult<()> {
        into_storage_result(Catalog::drop_table_field_analyzers(self, table_name))
    }

    fn load_table_field_analyzers(
        &self,
    ) -> StorageBackendResult<Vec<(String, String, String, String)>> {
        into_storage_result(Catalog::load_table_field_analyzers(self))
    }

    fn save_foreign_server(
        &self,
        name: &str,
        fdw_type: &str,
        options_json: &str,
    ) -> StorageBackendResult<()> {
        into_storage_result(Catalog::save_foreign_server(
            self,
            name,
            fdw_type,
            options_json,
        ))
    }

    fn drop_foreign_server(&self, name: &str) -> StorageBackendResult<()> {
        into_storage_result(Catalog::drop_foreign_server(self, name))
    }

    fn load_foreign_servers(&self) -> StorageBackendResult<Vec<(String, String, String)>> {
        into_storage_result(Catalog::load_foreign_servers(self))
    }

    fn save_foreign_table(
        &self,
        name: &str,
        server_name: &str,
        columns_json: &str,
        options_json: &str,
    ) -> StorageBackendResult<()> {
        into_storage_result(Catalog::save_foreign_table(
            self,
            name,
            server_name,
            columns_json,
            options_json,
        ))
    }

    fn drop_foreign_table(&self, name: &str) -> StorageBackendResult<()> {
        into_storage_result(Catalog::drop_foreign_table(self, name))
    }

    fn load_foreign_tables(&self) -> StorageBackendResult<Vec<ForeignTableRow>> {
        into_storage_result(Catalog::load_foreign_tables(self))
    }

    fn save_catalog_index(
        &self,
        name: &str,
        index_type: &str,
        table_name: &str,
        columns_json: &str,
        parameters_json: &str,
    ) -> StorageBackendResult<()> {
        into_storage_result(Catalog::save_catalog_index(
            self,
            name,
            index_type,
            table_name,
            columns_json,
            parameters_json,
        ))
    }

    fn drop_catalog_index(&self, name: &str) -> StorageBackendResult<()> {
        into_storage_result(Catalog::drop_catalog_index(self, name))
    }

    fn drop_catalog_indexes_for_table(&self, table_name: &str) -> StorageBackendResult<()> {
        into_storage_result(Catalog::drop_catalog_indexes_for_table(self, table_name))
    }

    fn load_catalog_indexes(&self) -> StorageBackendResult<Vec<CatalogIndexRow>> {
        into_storage_result(Catalog::load_catalog_indexes(self))
    }

    fn save_path_index(
        &self,
        graph_name: &str,
        label_sequences_json: &str,
    ) -> StorageBackendResult<()> {
        into_storage_result(Catalog::save_path_index(
            self,
            graph_name,
            label_sequences_json,
        ))
    }

    fn drop_path_index(&self, graph_name: &str) -> StorageBackendResult<()> {
        into_storage_result(Catalog::drop_path_index(self, graph_name))
    }

    fn load_path_indexes(&self) -> StorageBackendResult<Vec<(String, String)>> {
        into_storage_result(Catalog::load_path_indexes(self))
    }

    fn save_column_stats(&self, stats: ColumnStatsInput<'_>) -> StorageBackendResult<()> {
        into_storage_result(Catalog::save_column_stats(self, stats))
    }

    fn load_column_stats(&self, table_name: &str) -> StorageBackendResult<Vec<ColumnStatsRow>> {
        into_storage_result(Catalog::load_column_stats(self, table_name))
    }

    fn delete_column_stats(&self, table_name: &str) -> StorageBackendResult<()> {
        into_storage_result(Catalog::delete_column_stats(self, table_name))
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
    (
        4,
        r"
    CREATE TABLE IF NOT EXISTS _scoring_params (
        name TEXT PRIMARY KEY,
        params TEXT NOT NULL
    );
    ",
    ),
    (
        5,
        r"
    CREATE TABLE IF NOT EXISTS _graphs (
        name TEXT PRIMARY KEY
    );

    CREATE TABLE IF NOT EXISTS _graph_vertices (
        graph     TEXT NOT NULL,
        vertex_id INTEGER NOT NULL,
        body      TEXT NOT NULL,
        PRIMARY KEY (graph, vertex_id)
    );

    CREATE TABLE IF NOT EXISTS _graph_edges (
        graph   TEXT NOT NULL,
        edge_id INTEGER NOT NULL,
        body    TEXT NOT NULL,
        PRIMARY KEY (graph, edge_id)
    );
    CREATE INDEX IF NOT EXISTS _graph_edges_by_graph
        ON _graph_edges (graph);
    ",
    ),
    // Re-shape graph storage to mirror the canonical UQA implementation's UQA `storage/catalog`:
    // global vertex / edge tables keyed by id, a separate
    // `_graph_membership` table mapping each entity to one or more
    // named graphs, and the four supporting indexes the planner needs
    // for label-based lookups. The legacy v5 tables (denormalized by
    // graph name + JSON body) get dropped because no engine call site
    // reads them anymore.
    (
        6,
        r"
    DROP TABLE IF EXISTS _graphs;
    DROP TABLE IF EXISTS _graph_vertices;
    DROP TABLE IF EXISTS _graph_edges;

    CREATE TABLE IF NOT EXISTS _named_graphs (
        name TEXT PRIMARY KEY
    );

    CREATE TABLE IF NOT EXISTS _graph_vertices (
        vertex_id       INTEGER PRIMARY KEY,
        label           TEXT NOT NULL DEFAULT '',
        properties_json TEXT NOT NULL
    );

    CREATE TABLE IF NOT EXISTS _graph_edges (
        edge_id         INTEGER PRIMARY KEY,
        source_id       INTEGER NOT NULL,
        target_id       INTEGER NOT NULL,
        label           TEXT NOT NULL,
        properties_json TEXT NOT NULL
    );

    CREATE TABLE IF NOT EXISTS _graph_membership (
        entity_type TEXT NOT NULL,
        entity_id   INTEGER NOT NULL,
        graph_name  TEXT NOT NULL,
        PRIMARY KEY (entity_type, entity_id, graph_name)
    );

    CREATE INDEX IF NOT EXISTS _graph_vertices_label
        ON _graph_vertices (label);
    CREATE INDEX IF NOT EXISTS _graph_edges_out
        ON _graph_edges (source_id, label);
    CREATE INDEX IF NOT EXISTS _graph_edges_in
        ON _graph_edges (target_id, label);
    CREATE INDEX IF NOT EXISTS _graph_edges_label
        ON _graph_edges (label);
    ",
    ),
    // Persist the five engine-side registries that previously lived
    // only in `Engine`'s in-memory maps (named analyzers, table-field
    // analyzer overrides, foreign servers / tables, registered
    // indexes, graph path indexes). Tables and column shapes mirror
    // the canonical UQA implementation's UQA `storage/catalog` exactly.
    (
        7,
        r"
    CREATE TABLE IF NOT EXISTS _analyzers (
        name        TEXT PRIMARY KEY,
        config_json TEXT NOT NULL
    );

    CREATE TABLE IF NOT EXISTS _table_field_analyzers (
        table_name    TEXT NOT NULL,
        field         TEXT NOT NULL,
        phase         TEXT NOT NULL,
        analyzer_name TEXT NOT NULL,
        PRIMARY KEY (table_name, field, phase)
    );

    CREATE TABLE IF NOT EXISTS _foreign_servers (
        name     TEXT PRIMARY KEY,
        fdw_type TEXT NOT NULL,
        options  TEXT NOT NULL
    );

    CREATE TABLE IF NOT EXISTS _foreign_tables (
        name         TEXT PRIMARY KEY,
        server_name  TEXT NOT NULL,
        columns_json TEXT NOT NULL,
        options      TEXT NOT NULL
    );

    CREATE TABLE IF NOT EXISTS _catalog_indexes (
        name       TEXT PRIMARY KEY,
        index_type TEXT NOT NULL,
        table_name TEXT NOT NULL,
        columns    TEXT NOT NULL,
        parameters TEXT NOT NULL
    );

    CREATE TABLE IF NOT EXISTS _path_indexes (
        graph_name      TEXT PRIMARY KEY,
        label_sequences TEXT NOT NULL
    );
    ",
    ),
    // Persist per-column statistics produced by ANALYZE so that the
    // optimiser still has cardinality / range estimates after a
    // restart. Mirrors the canonical UQA implementation's `_column_stats` table.
    (
        8,
        r"
    CREATE TABLE IF NOT EXISTS _column_stats (
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
    );
    ",
    ),
    (
        9,
        r"
    CREATE TABLE IF NOT EXISTS _ivf_indexes (
        table_name          TEXT NOT NULL,
        field               TEXT NOT NULL,
        dimensions          INTEGER NOT NULL,
        nlist               INTEGER NOT NULL,
        nprobe              INTEGER NOT NULL,
        train_threshold     INTEGER NOT NULL,
        state               TEXT NOT NULL,
        trained_size        INTEGER NOT NULL,
        deletes_since_train INTEGER NOT NULL,
        vector_count        INTEGER NOT NULL,
        PRIMARY KEY (table_name, field)
    );

    CREATE TABLE IF NOT EXISTS _ivf_centroids (
        table_name  TEXT NOT NULL,
        field       TEXT NOT NULL,
        centroid_id INTEGER NOT NULL,
        vector      BLOB NOT NULL,
        PRIMARY KEY (table_name, field, centroid_id)
    );

    CREATE TABLE IF NOT EXISTS _ivf_assignments (
        table_name  TEXT NOT NULL,
        field       TEXT NOT NULL,
        doc_id      INTEGER NOT NULL,
        centroid_id INTEGER NOT NULL,
        PRIMARY KEY (table_name, field, doc_id)
    );
    CREATE INDEX IF NOT EXISTS _ivf_assignments_centroid_idx
        ON _ivf_assignments (table_name, field, centroid_id, doc_id);
    ",
    ),
    (
        10,
        r"
    CREATE TABLE IF NOT EXISTS _vectors_v10 (
        table_name     TEXT NOT NULL,
        field          TEXT NOT NULL,
        doc_id         INTEGER NOT NULL,
        vector_ordinal INTEGER NOT NULL DEFAULT 0,
        vector         BLOB NOT NULL,
        PRIMARY KEY (table_name, field, doc_id, vector_ordinal)
    );
    INSERT OR IGNORE INTO _vectors_v10
        (table_name, field, doc_id, vector_ordinal, vector)
        SELECT table_name, field, doc_id, 0, vector FROM _vectors;
    DROP TABLE IF EXISTS _vectors;
    ALTER TABLE _vectors_v10 RENAME TO _vectors;

    CREATE TABLE IF NOT EXISTS _ivf_assignments_v10 (
        table_name     TEXT NOT NULL,
        field          TEXT NOT NULL,
        doc_id         INTEGER NOT NULL,
        vector_ordinal INTEGER NOT NULL DEFAULT 0,
        centroid_id    INTEGER NOT NULL,
        PRIMARY KEY (table_name, field, doc_id, vector_ordinal)
    );
    INSERT OR IGNORE INTO _ivf_assignments_v10
        (table_name, field, doc_id, vector_ordinal, centroid_id)
        SELECT table_name, field, doc_id, 0, centroid_id FROM _ivf_assignments;
    DROP TABLE IF EXISTS _ivf_assignments;
    ALTER TABLE _ivf_assignments_v10 RENAME TO _ivf_assignments;
    CREATE INDEX IF NOT EXISTS _ivf_assignments_centroid_idx
        ON _ivf_assignments (table_name, field, centroid_id, doc_id, vector_ordinal);
    ",
    ),
    // Map logical btree indexes to compact durable postings. The engine
    // hydrates its in-memory B-tree from these rows on reopen instead of
    // reparsing every full document on the first indexed predicate.
    (
        11,
        r"
    CREATE TABLE IF NOT EXISTS _btree_indexes (
        table_name TEXT NOT NULL,
        field      TEXT NOT NULL,
        PRIMARY KEY (table_name, field)
    );

    CREATE TABLE IF NOT EXISTS _btree_index_entries (
        table_name TEXT NOT NULL,
        field      TEXT NOT NULL,
        doc_id     INTEGER NOT NULL,
        value_json TEXT NOT NULL,
        PRIMARY KEY (table_name, field, doc_id),
        FOREIGN KEY (table_name, field)
            REFERENCES _btree_indexes (table_name, field)
            ON UPDATE CASCADE ON DELETE CASCADE
    );
    CREATE INDEX IF NOT EXISTS _btree_index_value_idx
        ON _btree_index_entries (table_name, field, value_json, doc_id);
    CREATE INDEX IF NOT EXISTS _btree_index_doc_idx
        ON _btree_index_entries (table_name, doc_id);
    ",
    ),
    // `_postings` already has a unique auto-index over
    // `(table_name, field, term, doc_id)`. Its first three columns cover
    // term lookup, so the former `_postings_term_idx` duplicated every FTS
    // write without enabling a distinct access path.
    (
        12,
        r"
    DROP INDEX IF EXISTS _postings_term_idx;
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
    fn catalog_facade_trait_object_round_trips_table() {
        let cat = fresh();
        let facade: &dyn CatalogFacade = &cat;
        let schema = TableSchema {
            name: "facade_articles".into(),
            analyzer_json:
                "{\"tokenizer\":{\"type\":\"standard\"},\"token_filters\":[],\"char_filters\":[]}"
                    .into(),
            fts_fields: vec!["title".into()],
            vector_fields: vec![VectorFieldSchema {
                field: "embedding".into(),
                dimensions: 128,
            }],
            columns_json: String::new(),
        };
        facade.save_table(&schema).unwrap();
        let loaded = facade.load_tables().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name, "facade_articles");
    }

    #[test]
    fn migration_is_idempotent() {
        let mc = ManagedConnection::open_in_memory().unwrap();
        let _cat1 = Catalog::open(mc.clone()).unwrap();
        // Reopen on the same handle: should not re-run migrations or
        // raise an error.
        let _cat2 = Catalog::open(mc).unwrap();
    }

    #[test]
    fn migration_drops_redundant_postings_term_index() {
        let mc = ManagedConnection::open_in_memory().unwrap();
        let _current = Catalog::open(mc.clone()).unwrap();
        mc.with(|conn| {
            conn.execute(
                "CREATE INDEX _postings_term_idx
                 ON _postings (table_name, field, term)",
                [],
            )?;
            conn.execute(
                "UPDATE _metadata SET value = '11' WHERE key = 'schema_version'",
                [],
            )?;
            Ok(())
        })
        .unwrap();

        let _migrated = Catalog::open(mc.clone()).unwrap();
        mc.with(|conn| {
            let term_index_count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'index' AND name = '_postings_term_idx'",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(term_index_count, 0);

            let plan: String = conn.query_row(
                "EXPLAIN QUERY PLAN
                 SELECT doc_id, positions FROM _postings
                 WHERE table_name = 'docs' AND field = 'body' AND term = 'rust'
                 ORDER BY doc_id",
                [],
                |row| row.get(3),
            )?;
            assert!(
                plan.contains("sqlite_autoindex__postings_1"),
                "term lookup must use the composite primary-key index: {plan}"
            );
            Ok(())
        })
        .unwrap();
    }
}
