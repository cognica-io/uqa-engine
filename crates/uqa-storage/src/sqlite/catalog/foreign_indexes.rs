//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Foreign servers, foreign tables, catalog indexes, and path indexes.

use super::{
    params, Catalog, CatalogIndexRow, ForeignTableRow, RelationIdentity, RelationKind, Result,
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

    pub fn save_foreign_table(
        &self,
        relation: &RelationIdentity,
        server_name: &str,
        columns_json: &str,
        options_json: &str,
    ) -> Result<()> {
        self.conn.with_mut(|c| {
            let tx = c.savepoint()?;
            Self::claim_relation(&tx, relation, RelationKind::ForeignTable)?;
            tx.execute(
                "INSERT OR REPLACE INTO _foreign_tables \
                    (schema_name, relation_name, kind, server_name, columns_json, options) \
                 VALUES (?1, ?2, 'foreign_table', ?3, ?4, ?5)",
                params![
                    relation.schema,
                    relation.name,
                    server_name,
                    columns_json,
                    options_json
                ],
            )?;
            tx.commit()?;
            Ok(())
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
                "SELECT schema_name, relation_name, server_name, columns_json, options
                   FROM _foreign_tables ORDER BY schema_name, relation_name",
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
                let (schema, name, server, cols, opts) = row?;
                out.push(ForeignTableRow {
                    relation: RelationIdentity::new(schema, name),
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
}
