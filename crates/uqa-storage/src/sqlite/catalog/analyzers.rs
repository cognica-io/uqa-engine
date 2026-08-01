//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Named and per-field analyzer configuration.

use super::{params, Catalog, Result};

impl Catalog {
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

    pub fn replace_table_field_analyzer(
        &self,
        table_name: &str,
        field: &str,
        phase: &str,
        analyzer_name: &str,
    ) -> Result<()> {
        self.conn.with_mut(|c| {
            let tx = c.savepoint()?;
            tx.execute(
                "DELETE FROM _table_field_analyzers
                  WHERE table_name = ?1 AND field = ?2",
                params![table_name, field],
            )?;
            tx.execute(
                "INSERT INTO _table_field_analyzers
                    (table_name, field, phase, analyzer_name)
                 VALUES (?1, ?2, ?3, ?4)",
                params![table_name, field, phase, analyzer_name],
            )?;
            tx.commit()?;
            Ok(())
        })
    }

    pub fn drop_table_field_analyzer_field(&self, table_name: &str, field: &str) -> Result<()> {
        self.conn.with(|c| {
            c.execute(
                "DELETE FROM _table_field_analyzers
                  WHERE table_name = ?1 AND field = ?2",
                params![table_name, field],
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
}
