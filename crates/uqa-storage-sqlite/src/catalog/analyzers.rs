//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Named and per-field analyzer configuration.

use super::native::{text, FieldWrite};
use super::{params, Catalog, Result};
use crate::mvcc::native::NativeRecordFamily as Family;
use rusqlite::types::ValueRef;

impl Catalog {
    // -- Named analyzers ---------------------------------------------------

    /// Persist a named analyzer configuration.
    pub fn save_analyzer(&self, name: &str, config_json: &str) -> Result<()> {
        if self
            .put_native_named(
                Family::Analyzers,
                &[text(name), text(config_json), ValueRef::Null],
            )?
            .is_some()
        {
            return Ok(());
        }
        self.conn.with(|c| {
            c.execute(
                "INSERT OR REPLACE INTO _analyzers (name, config_json) VALUES (?1, ?2)",
                params![name, config_json],
            )?;
            Ok(())
        })
    }

    pub fn drop_analyzer(&self, name: &str) -> Result<()> {
        if self.drop_native_named(Family::Analyzers, name)?.is_some() {
            return Ok(());
        }
        self.conn.with(|c| {
            c.execute("DELETE FROM _analyzers WHERE name = ?1", params![name])?;
            Ok(())
        })
    }

    pub fn load_analyzers(&self) -> Result<Vec<(String, String)>> {
        if let Some(analyzers) = self.load_native_named(Family::Analyzers, 1, false)? {
            return Ok(analyzers);
        }
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

    /// Persist a `(table, field, phase) -> analyzer_name` mapping.
    pub fn save_table_field_analyzer(
        &self,
        table_name: &str,
        field: &str,
        phase: &str,
        analyzer_name: &str,
    ) -> Result<()> {
        if self
            .write_native_analyzer_field(
                table_name,
                field,
                phase,
                analyzer_name,
                FieldWrite::Legacy,
            )?
            .is_some()
        {
            return Ok(());
        }
        self.conn.with_mut(|connection| {
            let tx = connection.savepoint()?;
            tx.execute(
                "UPDATE _table_field_analyzers SET binding_json = NULL WHERE table_name = ?1 AND field = ?2",
                params![table_name, field],
            )?;
            tx.execute(
                "INSERT OR REPLACE INTO _table_field_analyzers \
                    (table_name, field, phase, analyzer_name) \
                 VALUES (?1, ?2, ?3, ?4)",
                params![table_name, field, phase, analyzer_name],
            )?;
            tx.commit()?;
            Ok(())
        })
    }

    pub fn drop_table_field_analyzers(&self, table_name: &str) -> Result<()> {
        if self
            .drop_native_analyzer_fields(table_name, None)?
            .is_some()
        {
            return Ok(());
        }
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
        if self
            .write_native_analyzer_field(
                table_name,
                field,
                phase,
                analyzer_name,
                FieldWrite::Replace(None),
            )?
            .is_some()
        {
            return Ok(());
        }
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
        if self
            .drop_native_analyzer_fields(table_name, Some(field))?
            .is_some()
        {
            return Ok(());
        }
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
        if let Some(fields) = self.load_native_analyzer_fields()? {
            return Ok(fields);
        }
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

impl Catalog {
    pub fn save_analyzer_revision(
        &self,
        name: &str,
        config_json: &str,
        descriptor_json: &str,
    ) -> Result<()> {
        if self
            .put_native_named(
                Family::Analyzers,
                &[text(name), text(config_json), text(descriptor_json)],
            )?
            .is_some()
        {
            return Ok(());
        }
        self.conn.with(|connection| {
            connection.execute("INSERT OR REPLACE INTO _analyzers (name, config_json, descriptor_json) VALUES (?1, ?2, ?3)", params![name, config_json, descriptor_json])?;
            Ok(())
        })
    }

    pub fn load_analyzer_descriptors(&self) -> Result<Vec<(String, String)>> {
        if let Some(descriptors) = self.load_native_named(Family::Analyzers, 2, true)? {
            return Ok(descriptors);
        }
        self.conn.with(|connection| {
            let mut statement = connection.prepare("SELECT name, descriptor_json FROM _analyzers WHERE descriptor_json IS NOT NULL ORDER BY name")?;
            let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?.collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(rows)
        })
    }

    pub fn replace_table_field_analyzer_binding(
        &self,
        table: &str,
        field: &str,
        phase: &str,
        name: &str,
        binding_json: &str,
    ) -> Result<()> {
        if self
            .write_native_analyzer_field(
                table,
                field,
                phase,
                name,
                FieldWrite::Replace(Some(binding_json)),
            )?
            .is_some()
        {
            return Ok(());
        }
        self.conn.with_mut(|connection| {
            let tx = connection.savepoint()?;
            tx.execute("DELETE FROM _table_field_analyzers WHERE table_name = ?1 AND field = ?2", params![table, field])?;
            tx.execute("INSERT INTO _table_field_analyzers (table_name, field, phase, analyzer_name, binding_json) VALUES (?1, ?2, ?3, ?4, ?5)", params![table, field, phase, name, binding_json])?;
            tx.commit()?;
            Ok(())
        })
    }

    pub fn load_table_field_analyzer_bindings(&self) -> Result<Vec<(String, String, String)>> {
        if let Some(bindings) = self.load_native_analyzer_bindings()? {
            return Ok(bindings);
        }
        self.conn.with(|connection| {
            let mut statement = connection.prepare("SELECT table_name, field, binding_json FROM _table_field_analyzers WHERE binding_json IS NOT NULL ORDER BY table_name, field")?;
            let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?.collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(rows)
        })
    }
}
