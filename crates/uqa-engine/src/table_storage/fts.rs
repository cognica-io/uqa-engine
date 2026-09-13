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
        let existing = self
            .durable
            .table_field_analyzers
            .read()
            .get(&(table_name.clone(), field.clone()))
            .cloned();
        let uses_default = analyzer.is_none() && existing.is_none();
        let candidate = if let Some(name) = analyzer {
            let name = name.trim();
            let revision = self.resolve_analyzer_revision(name)?;
            let previous = existing.unwrap_or_else(|| {
                uqa_storage::FieldAnalyzerBinding::unassigned(revision.clone(), revision.clone())
            });
            match previous.owner {
                uqa_storage::AnalyzerBindingOwner::Field
                    if previous.index.name.is_some() || previous.search.name.is_some() =>
                {
                    return Err(
                        "GIN analyzer option competes with an existing field assignment".into(),
                    )
                }
                uqa_storage::AnalyzerBindingOwner::Gin
                    if previous.index.name.as_deref() != Some(name)
                        || previous.index.compiled.descriptor().fingerprint()
                            != revision.descriptor().fingerprint() =>
                {
                    return Err("GIN analyzer option competes with an existing GIN revision".into())
                }
                _ => {}
            }
            previous.assigned(
                name,
                revision,
                AnalyzerPhase::Both,
                uqa_storage::AnalyzerBindingOwner::Gin,
            )
        } else {
            self.current_field_analyzer_binding(&table_name, &t, &field)?
        };
        {
            let mut fts = t.fts_fields.write();
            if !fts.contains(&field) {
                fts.push(field.clone());
            }
        }
        let documents = Self::project_fts_sources(&t)?;
        let phase = if analyzer.is_some() {
            AnalyzerPhase::Both
        } else {
            AnalyzerPhase::Index
        };
        t.inverted_index
            .write()
            .rebuild_with_analyzer_revision(
                &field,
                candidate.index.compiled.clone(),
                phase,
                documents,
            )
            .map_err(|error| format!("add_fts_field: {error}"))?;
        candidate
            .install(&field, t.inverted_index.write().as_mut())
            .map_err(|error| error.to_string())?;
        if uses_default {
            *t.analyzer.write() = candidate
                .index
                .compiled
                .descriptor()
                .configuration()
                .map_err(|error| error.to_string())?;
        }
        self.persist_field_analyzer_binding(&table_name, &field, &candidate)?;
        self.durable
            .table_field_analyzers
            .write()
            .insert((table_name.clone(), field), candidate);
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

        t.fts_fields.write().retain(|candidate| candidate != field);
        Self::rebuild_fts_index(&t)
            .map_err(|err| format!("rebuild FTS index for `{table_name}`: {err}"))?;
        t.inverted_index
            .write()
            .remove_field_analyzers(field)
            .map_err(|err| format!("remove FTS analyzer `{table_name}`.`{field}`: {err}"))?;

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

impl Engine {
    /// Release the last explicit GIN analyzer owner while another GIN still keeps the field indexed.
    pub(crate) fn release_fts_analyzer_owner(
        &self,
        table: &str,
        field: &str,
    ) -> Result<(), String> {
        let table_name = self
            .try_resolve_table_name(table)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("unknown table `{table}`"))?;
        let t = self
            .try_table(&table_name)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("unknown table `{table}`"))?;
        let Some(previous) = self
            .durable
            .table_field_analyzers
            .read()
            .get(&(table_name.clone(), field.to_owned()))
            .cloned()
        else {
            return Ok(());
        };
        if previous.owner != uqa_storage::AnalyzerBindingOwner::Gin {
            return Ok(());
        }
        let revision = t
            .analyzer
            .read()
            .clone()
            .compile()
            .map_err(|error| error.to_string())?;
        let binding =
            uqa_storage::FieldAnalyzerBinding::unassigned(revision.clone(), revision.clone());
        let documents = Self::project_fts_sources(&t)?;
        t.inverted_index
            .write()
            .rebuild_with_analyzer_revision(field, revision.clone(), AnalyzerPhase::Both, documents)
            .map_err(|error| error.to_string())?;
        *t.analyzer.write() = revision
            .descriptor()
            .configuration()
            .map_err(|error| error.to_string())?;
        self.persist_field_analyzer_binding(&table_name, field, &binding)?;
        self.durable
            .table_field_analyzers
            .write()
            .insert((table_name.clone(), field.to_owned()), binding);
        if self.is_persistent() && t.persistence != uqa_sql::ast::RelationPersistence::Temporary {
            self.try_save_table_schema(&table_name, &t)
                .map_err(|error| error.to_string())?;
            self.note_table_catalog_changed();
            self.note_catalog_registry_changed();
        }
        Ok(())
    }
}
