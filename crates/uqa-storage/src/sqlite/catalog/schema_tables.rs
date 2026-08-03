//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Metadata, schema, table, and column lifecycle.

use super::{
    columns_json_references, delete_table_rows_if_exists, drop_fts_aux_tables_for_field,
    drop_fts_aux_tables_for_table, migration_relation, params,
    rename_btree_field_rows_or_keep_existing, rename_field_rows_or_keep_existing,
    rename_fts_aux_tables_for_field, renamed_columns_json, table_exists,
    update_btree_table_name_rows_if_exists, update_table_name_rows_if_exists, Catalog,
    OptionalExtension, RelationIdentity, RelationKind, Result, SQLiteError, TableSchema,
    VectorFieldSchema,
};

impl Catalog {
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

    pub fn save_schema(&self, name: &str) -> Result<()> {
        self.conn.with(|c| {
            c.execute(
                "INSERT OR IGNORE INTO _schemas (name) VALUES (?1)",
                params![name],
            )?;
            Ok(())
        })
    }

    pub fn drop_schema(&self, name: &str) -> Result<()> {
        self.conn.with(|c| {
            let relation_count: i64 = c.query_row(
                "SELECT COUNT(*) FROM _relations WHERE schema_name = ?1",
                params![name],
                |row| row.get(0),
            )?;
            if relation_count != 0 {
                return Err(SQLiteError::StorageBackend(format!(
                    "schema `{name}` still owns catalog relations"
                )));
            }
            c.execute("DELETE FROM _schemas WHERE name = ?1", params![name])?;
            Ok(())
        })
    }

    pub fn load_schemas(&self) -> Result<Vec<String>> {
        self.conn.with(|c| {
            let mut stmt = c.prepare("SELECT name FROM _schemas ORDER BY name")?;
            let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    pub fn save_table(&self, schema: &TableSchema) -> Result<()> {
        let analyzer = schema.analyzer_json.clone();
        let fts = serde_json::to_string(&schema.fts_fields)?;
        let vectors = serde_json::to_string(&schema.vector_fields)?;
        let columns = schema.columns_json.clone();
        let constraints = schema.constraints_json.clone();
        self.conn.with_mut(|c| {
            let tx = c.savepoint()?;
            Self::claim_relation(&tx, &schema.relation, RelationKind::Table)?;
            tx.execute(
                "INSERT OR REPLACE INTO _tables
                    (schema_name, relation_name, kind, analyzer, fts_fields,
                     vector_fields, columns, constraints)
                 VALUES (?1, ?2, 'table', ?3, ?4, ?5, ?6, ?7)",
                params![
                    schema.relation.schema,
                    schema.relation.name,
                    analyzer,
                    fts,
                    vectors,
                    columns,
                    constraints
                ],
            )?;
            tx.commit()?;
            Ok(())
        })
    }

    pub fn load_tables(&self) -> Result<Vec<TableSchema>> {
        self.conn.with(|c| {
            let mut stmt = c.prepare(
                "SELECT schema_name, relation_name, analyzer, fts_fields,
                        vector_fields, columns, constraints
                   FROM _tables ORDER BY schema_name, relation_name",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, Option<String>>(5)?,
                    r.get::<_, String>(6)?,
                ))
            })?;
            let mut out = Vec::new();
            for row in rows {
                let (
                    schema_name,
                    relation_name,
                    analyzer_json,
                    fts_str,
                    vec_str,
                    cols_opt,
                    constraints_json,
                ) = row?;
                let fts_fields: Vec<String> = serde_json::from_str(&fts_str)?;
                let vector_fields: Vec<VectorFieldSchema> = serde_json::from_str(&vec_str)?;
                out.push(TableSchema {
                    relation: RelationIdentity::new(schema_name, relation_name),
                    analyzer_json,
                    fts_fields,
                    vector_fields,
                    columns_json: cols_opt.unwrap_or_default(),
                    constraints_json,
                });
            }
            Ok(out)
        })
    }

    pub fn drop_table(&self, name: &str) -> Result<()> {
        let relation = migration_relation(name)?;
        self.conn.with_mut(|c| {
            let tx = c.savepoint()?;
            tx.execute(
                "DELETE FROM _tables WHERE schema_name = ?1 AND relation_name = ?2",
                params![relation.schema, relation.name],
            )?;
            Self::release_relation(&tx, &relation, RelationKind::Table)?;
            tx.commit()?;
            Ok(())
        })
    }

    /// Wipe the rows owned by `table` from the per-table data tables
    /// (`_documents`, `_postings`, `_doc_lengths`, `_field_stats`,
    /// `_vectors`, IVF/HNSW metadata). Run after [`Catalog::drop_table`]
    /// when the engine drops the table from its in-memory registry as
    /// well.
    pub fn purge_table_data(&self, name: &str) -> Result<()> {
        let relation = migration_relation(name)?;
        let storage_names = relation.canonical_and_legacy_public_names();
        self.conn.with_mut(|c| {
            let tx = c.savepoint()?;
            for storage_name in &storage_names {
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
                    "_hnsw_indexes",
                    "_hnsw_nodes",
                    "_hnsw_edges",
                    "_column_stats",
                    "_btree_index_entries",
                    "_btree_indexes",
                ] {
                    delete_table_rows_if_exists(&tx, table, storage_name)?;
                }
                drop_fts_aux_tables_for_table(&tx, storage_name)?;
            }
            tx.commit()?;
            Ok(())
        })
    }

    pub fn drop_table_and_data(&self, name: &str) -> Result<()> {
        let relation = migration_relation(name)?;
        let storage_names = relation.canonical_and_legacy_public_names();
        self.conn.with_mut(|c| {
            let tx = c.savepoint()?;
            tx.execute(
                "DELETE FROM _tables WHERE schema_name = ?1 AND relation_name = ?2",
                params![relation.schema, relation.name],
            )?;
            for storage_name in &storage_names {
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
                    "_hnsw_indexes",
                    "_hnsw_nodes",
                    "_hnsw_edges",
                    "_column_stats",
                    "_btree_index_entries",
                    "_btree_indexes",
                ] {
                    delete_table_rows_if_exists(&tx, table, storage_name)?;
                }
                tx.execute(
                    "DELETE FROM _table_field_analyzers WHERE table_name = ?1",
                    params![storage_name],
                )?;
                tx.execute(
                    "DELETE FROM _catalog_indexes WHERE table_name = ?1",
                    params![storage_name],
                )?;
                drop_fts_aux_tables_for_table(&tx, storage_name)?;
            }
            Self::release_relation(&tx, &relation, RelationKind::Table)?;
            tx.commit()?;
            Ok(())
        })
    }

    pub fn rename_table_data(&self, from: &str, to: &str) -> Result<()> {
        let from_relation = migration_relation(from)?;
        let to_relation = migration_relation(to)?;
        if from_relation == to_relation {
            return Ok(());
        }
        self.conn.with_mut(|c| {
            let tx = c.savepoint()?;
            Self::claim_relation(&tx, &to_relation, RelationKind::Table)?;
            let updated = tx.execute(
                "UPDATE _tables
                    SET schema_name = ?3, relation_name = ?4
                  WHERE schema_name = ?1 AND relation_name = ?2",
                params![
                    from_relation.schema,
                    from_relation.name,
                    to_relation.schema,
                    to_relation.name
                ],
            )?;
            if updated == 0 {
                return Err(SQLiteError::StorageBackend(format!(
                    "table `{from}` does not exist"
                )));
            }
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
                "_hnsw_indexes",
                "_hnsw_nodes",
                "_hnsw_edges",
                "_column_stats",
                "_table_field_analyzers",
                "_catalog_indexes",
            ] {
                update_table_name_rows_if_exists(&tx, table, from, to)?;
            }
            update_btree_table_name_rows_if_exists(&tx, from, to)?;
            drop_fts_aux_tables_for_table(&tx, from)?;
            Self::release_relation(&tx, &from_relation, RelationKind::Table)?;
            tx.commit()?;
            Ok(())
        })
    }

    pub fn drop_column_data(&self, table_name: &str, column_name: &str) -> Result<()> {
        let indexes = self.catalog_indexes_referencing_column(table_name, column_name)?;
        self.conn.with_mut(|c| {
            let tx = c.savepoint()?;
            if table_exists(&tx, "_document_blobs")? {
                tx.execute(
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
                "_hnsw_indexes",
                "_hnsw_nodes",
                "_hnsw_edges",
                "_btree_index_entries",
                "_btree_indexes",
            ] {
                tx.execute(
                    &format!("DELETE FROM {table} WHERE table_name = ?1 AND field = ?2"),
                    params![table_name, column_name],
                )?;
            }
            tx.execute(
                "DELETE FROM _column_stats WHERE table_name = ?1 AND column_name = ?2",
                params![table_name, column_name],
            )?;
            tx.execute(
                "DELETE FROM _table_field_analyzers WHERE table_name = ?1 AND field = ?2",
                params![table_name, column_name],
            )?;
            for index_name in indexes {
                tx.execute(
                    "DELETE FROM _catalog_indexes WHERE name = ?1",
                    params![index_name],
                )?;
            }
            drop_fts_aux_tables_for_field(&tx, table_name, column_name)?;
            tx.commit()?;
            Ok(())
        })
    }

    pub fn rename_column_data(&self, table_name: &str, from: &str, to: &str) -> Result<()> {
        let index_updates = self.catalog_index_column_renames(table_name, from, to)?;
        self.conn.with_mut(|c| {
            let tx = c.savepoint()?;
            rename_field_rows_or_keep_existing(
                &tx,
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
                "_hnsw_indexes",
                "_hnsw_nodes",
                "_hnsw_edges",
            ] {
                rename_field_rows_or_keep_existing(&tx, table, "field", table_name, from, to)?;
            }
            rename_btree_field_rows_or_keep_existing(&tx, table_name, from, to)?;
            rename_field_rows_or_keep_existing(
                &tx,
                "_column_stats",
                "column_name",
                table_name,
                from,
                to,
            )?;
            rename_field_rows_or_keep_existing(
                &tx,
                "_table_field_analyzers",
                "field",
                table_name,
                from,
                to,
            )?;
            for (index_name, columns_json) in index_updates {
                tx.execute(
                    "UPDATE _catalog_indexes
                        SET columns = ?2
                      WHERE name = ?1",
                    params![index_name, columns_json],
                )?;
            }
            rename_fts_aux_tables_for_field(&tx, table_name, from, to)?;
            tx.commit()?;
            Ok(())
        })
    }

    pub(super) fn catalog_indexes_referencing_column(
        &self,
        table_name: &str,
        column_name: &str,
    ) -> Result<Vec<String>> {
        let mut out = Vec::new();
        for row in self.load_catalog_indexes()? {
            if row.table_name == table_name
                && columns_json_references(&row.columns_json, column_name)?
            {
                out.push(row.name);
            }
        }
        Ok(out)
    }

    pub(super) fn catalog_index_column_renames(
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
            if let Some(columns_json) = renamed_columns_json(&row.columns_json, from, to)? {
                out.push((row.name, columns_json));
            }
        }
        Ok(out)
    }
}
