//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Foreign servers, foreign tables, catalog indexes, and path indexes.

use super::{
    params, Catalog, CatalogIndexRow, ForeignTableRow, RelationIdentity, RelationKind, Result,
    SQLiteError, TableAclEntry,
};

impl Catalog {
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

    pub fn save_foreign_table(&self, row: &ForeignTableRow) -> Result<()> {
        self.conn.with_mut(|c| {
            let tx = c.savepoint()?;
            Self::claim_relation(&tx, &row.relation, RelationKind::ForeignTable)?;
            let acl_json = row.acl.as_deref().map(serde_json::to_string).transpose()?;
            let column_acls_json = serde_json::to_string(&row.column_acls)?;
            tx.execute(
                "INSERT OR REPLACE INTO _foreign_tables \
                    (schema_name, relation_name, kind, role_owner, acl_json, column_acls_json, server_name, columns_json, options) \
                 VALUES (?1, ?2, 'foreign_table', ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    row.relation.schema,
                    row.relation.name,
                    row.role_owner,
                    acl_json,
                    column_acls_json,
                    row.server_name,
                    row.columns_json,
                    row.options_json
                ],
            )?;
            tx.commit()?;
            Ok(())
        })
    }

    pub fn update_foreign_table_security(
        &self,
        relation: &RelationIdentity,
        role_owner: &str,
        acl: Option<&[TableAclEntry]>,
        column_acls: &std::collections::BTreeMap<String, Vec<TableAclEntry>>,
    ) -> Result<bool> {
        self.conn.with_mut(|connection| {
            let acl_json = acl.map(serde_json::to_string).transpose()?;
            let column_acls_json = serde_json::to_string(column_acls)?;
            Ok(connection.execute(
                "UPDATE _foreign_tables
                    SET role_owner = ?3, acl_json = ?4, column_acls_json = ?5
                  WHERE schema_name = ?1 AND relation_name = ?2",
                params![
                    relation.schema,
                    relation.name,
                    role_owner,
                    acl_json,
                    column_acls_json
                ],
            )? != 0)
        })
    }

    pub fn drop_foreign_table(&self, relation: &RelationIdentity) -> Result<()> {
        self.conn.with_mut(|c| {
            let tx = c.savepoint()?;
            let removed = tx.execute(
                "DELETE FROM _foreign_tables
                  WHERE schema_name = ?1 AND relation_name = ?2",
                params![relation.schema, relation.name],
            )? != 0;
            if removed {
                Self::release_relation(&tx, relation, RelationKind::ForeignTable)?;
            }
            tx.commit()?;
            Ok(())
        })
    }

    pub fn load_foreign_tables(&self) -> Result<Vec<ForeignTableRow>> {
        self.conn.with(|c| {
            let mut stmt = c.prepare(
                "SELECT schema_name, relation_name, role_owner, acl_json, column_acls_json, server_name, columns_json, options
                   FROM _foreign_tables ORDER BY schema_name, relation_name",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, String>(6)?,
                    r.get::<_, String>(7)?,
                ))
            })?;
            let mut out = Vec::new();
            for row in rows {
                let (schema, name, owner, acl_json, column_acls_json, server, cols, opts) = row?;
                out.push(ForeignTableRow {
                    relation: RelationIdentity::new(schema, name),
                    role_owner: owner,
                    acl: acl_json
                        .as_deref()
                        .map(serde_json::from_str)
                        .transpose()?,
                    column_acls: serde_json::from_str(&column_acls_json)?,
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
        relation: &RelationIdentity,
        index_type: &str,
        table_name: &str,
        columns_json: &str,
        parameters_json: &str,
    ) -> Result<()> {
        let table =
            RelationIdentity::from_legacy_name(table_name).map_err(SQLiteError::StorageBackend)?;
        if relation.schema != table.schema {
            return Err(SQLiteError::StorageBackend(format!(
                "catalog index `{}` cannot belong to a different schema than table `{}`",
                relation.qualified_name(),
                table.qualified_name()
            )));
        }
        self.conn.with_mut(|c| {
            let tx = c.savepoint()?;
            Self::claim_relation(&tx, relation, RelationKind::Index)?;
            tx.execute(
                "INSERT INTO _catalog_indexes
                    (schema_name, relation_name, kind, index_type, table_schema_name,
                     table_relation_name, columns, parameters)
                 VALUES (?1, ?2, 'index', ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(schema_name, relation_name) DO UPDATE SET
                     index_type = excluded.index_type,
                     table_schema_name = excluded.table_schema_name,
                     table_relation_name = excluded.table_relation_name,
                     columns = excluded.columns,
                     parameters = excluded.parameters",
                params![
                    relation.schema,
                    relation.name,
                    index_type,
                    table.schema,
                    table.name,
                    columns_json,
                    parameters_json
                ],
            )?;
            tx.commit()?;
            Ok(())
        })
    }

    pub fn drop_catalog_index(&self, relation: &RelationIdentity) -> Result<()> {
        self.conn.with_mut(|c| {
            let tx = c.savepoint()?;
            tx.execute(
                "DELETE FROM _catalog_indexes
                  WHERE schema_name = ?1 AND relation_name = ?2",
                params![relation.schema, relation.name],
            )?;
            Self::release_relation(&tx, relation, RelationKind::Index)?;
            tx.commit()?;
            Ok(())
        })
    }

    pub fn drop_catalog_indexes_for_table(&self, table_name: &str) -> Result<()> {
        let table =
            RelationIdentity::from_legacy_name(table_name).map_err(SQLiteError::StorageBackend)?;
        self.conn.with_mut(|c| {
            let tx = c.savepoint()?;
            Self::drop_catalog_index_rows_for_table(&tx, &table)?;
            tx.commit()?;
            Ok(())
        })
    }

    pub(in crate::sqlite::catalog) fn drop_catalog_index_rows_for_table(
        conn: &rusqlite::Connection,
        table: &RelationIdentity,
    ) -> Result<()> {
        let indexes = {
            let mut statement = conn.prepare(
                "SELECT schema_name, relation_name
                   FROM _catalog_indexes
                  WHERE table_schema_name = ?1 AND table_relation_name = ?2",
            )?;
            let indexes = statement
                .query_map(params![table.schema, table.name], |row| {
                    Ok(RelationIdentity::new(
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            indexes
        };
        for index in indexes {
            conn.execute(
                "DELETE FROM _catalog_indexes
                  WHERE schema_name = ?1 AND relation_name = ?2",
                params![index.schema, index.name],
            )?;
            Self::release_relation(conn, &index, RelationKind::Index)?;
        }
        Ok(())
    }

    pub fn load_catalog_indexes(&self) -> Result<Vec<CatalogIndexRow>> {
        self.conn.with(|c| {
            let mut stmt = c.prepare(
                "SELECT schema_name, relation_name, index_type,
                        table_schema_name, table_relation_name, columns, parameters
                   FROM _catalog_indexes ORDER BY schema_name, relation_name",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, String>(6)?,
                ))
            })?;
            let mut out = Vec::new();
            for row in rows {
                let (schema, name, ty, table_schema, table_name, cols, params_json) = row?;
                out.push(CatalogIndexRow {
                    relation: RelationIdentity::new(schema, name),
                    index_type: ty,
                    table_name: RelationIdentity::new(table_schema, table_name).qualified_name(),
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
}
