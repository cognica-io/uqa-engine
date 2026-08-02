//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Full-text field registration and removal.

use super::{AnalyzerPhase, Engine, FieldName};

impl Engine {
    /// Append `field` to the table's FTS field list. Existing rows are
    /// indexed immediately so SQL `CREATE INDEX USING gin` behaves like a
    /// real secondary-index build rather than a metadata-only toggle.
    pub fn add_fts_field(&self, table: &str, field: FieldName) -> Result<(), String> {
        self.add_fts_field_with_analyzer(table, field, None)
    }

    /// Same as [`Engine::add_fts_field`], but allows registering a
    /// per-field analyzer name (e.g. `standard_cjk`). When `None`, the
    /// table-level analyzer continues to apply.
    pub fn add_fts_field_with_analyzer(
        &self,
        table: &str,
        field: FieldName,
        analyzer: Option<&str>,
    ) -> Result<(), String> {
        self.with_implicit_string_transaction(|engine| {
            engine.add_fts_field_with_analyzer_inner(table, field, analyzer)
        })
    }

    pub(super) fn add_fts_field_with_analyzer_inner(
        &self,
        table: &str,
        field: FieldName,
        analyzer: Option<&str>,
    ) -> Result<(), String> {
        let table_name = self
            .try_resolve_table_name(table)
            .map_err(|err| format!("resolve table `{table}`: {err}"))?
            .ok_or_else(|| format!("unknown table `{table}`"))?;
        let t = self
            .try_table(table)
            .map_err(|err| format!("resolve table `{table}`: {err}"))?
            .ok_or_else(|| format!("unknown table `{table}`"))?;
        if let Some(analyzer_name) = analyzer {
            let analyzer = self.resolve_analyzer(analyzer_name)?;
            t.inverted_index
                .write()
                .set_field_analyzer(&field, analyzer, AnalyzerPhase::Both)
                .map_err(|e| format!("add_fts_field: {e}"))?;
            self.durable.table_field_analyzers.write().insert(
                (table_name.clone(), field.clone()),
                (analyzer_name.to_string(), "both".to_string()),
            );
            if let Some(catalog) = self.storage.catalog.as_ref() {
                catalog
                    .replace_table_field_analyzer(&table_name, &field, "both", analyzer_name)
                    .map_err(|err| format!("persist FTS analyzer: {err}"))?;
            }
        }
        {
            let mut fts = t.fts_fields.write();
            if !fts.contains(&field) {
                fts.push(field);
            }
        }
        Self::rebuild_fts_index(&t)?;
        if self.is_persistent() {
            self.try_save_table_schema(&table_name, &t)
                .map_err(|err| format!("persist FTS schema `{table_name}`: {err}"))?;
        }
        Ok(())
    }

    /// Remove a field from the physical FTS index and from every piece of
    /// analyzer/schema metadata that makes the field searchable.  Callers
    /// must first establish that no other logical GIN index still references
    /// the field.
    pub(crate) fn drop_fts_field(&self, table: &str, field: &str) -> Result<(), String> {
        self.with_implicit_string_transaction(|engine| engine.drop_fts_field_inner(table, field))
    }

    pub(super) fn drop_fts_field_inner(&self, table: &str, field: &str) -> Result<(), String> {
        let table_name = self
            .try_resolve_table_name(table)
            .map_err(|err| format!("resolve table `{table}`: {err}"))?
            .ok_or_else(|| format!("unknown table `{table}`"))?;
        let t = self
            .try_table(&table_name)
            .map_err(|err| format!("resolve table `{table_name}`: {err}"))?
            .ok_or_else(|| format!("unknown table `{table_name}`"))?;

        if !t
            .fts_fields
            .read()
            .iter()
            .any(|candidate| candidate == field)
        {
            return Err(format!(
                "field `{table_name}`.`{field}` is not registered in the physical FTS index"
            ));
        }

        t.inverted_index
            .write()
            .remove_field_analyzers(field)
            .map_err(|err| format!("remove FTS analyzer `{table_name}`.`{field}`: {err}"))?;
        t.fts_fields.write().retain(|candidate| candidate != field);
        Self::rebuild_fts_index(&t)
            .map_err(|err| format!("rebuild FTS index for `{table_name}`: {err}"))?;

        if let Some(catalog) = self.storage.catalog.as_ref() {
            catalog
                .drop_table_field_analyzer_field(&table_name, field)
                .map_err(|err| {
                    format!("drop persisted FTS analyzer `{table_name}`.`{field}`: {err}")
                })?;
        }
        self.durable
            .table_field_analyzers
            .write()
            .remove(&(table_name.clone(), field.to_string()));
        if self.is_persistent() {
            self.try_save_table_schema(&table_name, &t)
                .map_err(|err| format!("persist FTS schema `{table_name}`: {err}"))?;
            self.note_catalog_registry_changed();
        }
        Ok(())
    }
}
