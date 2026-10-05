//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Full-text field registration and removal.

use super::{AnalyzerPhase, Engine, FieldName};
use uqa_execution::storage_errors::storage_error;
use uqa_sql::SQLError;

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
            engine
                .add_fts_field_with_analyzer_inner(table, field, analyzer)
                .map_err(|error| error.to_string())
        })
    }

    pub(crate) fn add_fts_field_with_analyzer_inner(
        &self,
        table: &str,
        field: FieldName,
        analyzer: Option<&str>,
    ) -> Result<(), SQLError> {
        let table_name = self
            .try_resolve_table_name(table)
            .map_err(|error| storage_error("resolve FTS table", &error))?
            .ok_or_else(|| SQLError::UnknownTable(table.into()))?;
        let t = self
            .try_table(table)
            .map_err(|error| storage_error("resolve FTS table", &error))?
            .ok_or_else(|| SQLError::UnknownTable(table.into()))?;
        let existing = self
            .durable
            .table_field_analyzers
            .read()
            .get(&(table_name.clone(), field.clone()))
            .cloned();
        let uses_default = analyzer.is_none() && existing.is_none();
        let candidate = if let Some(name) = analyzer {
            let name = name.trim();
            let revision = self
                .resolve_analyzer_revision(name)
                .map_err(|error| storage_error("resolve FTS analyzer", &error))?;
            let previous = existing.unwrap_or_else(|| {
                uqa_storage::FieldAnalyzerBinding::unassigned(revision.clone(), revision.clone())
            });
            match previous.owner {
                uqa_storage::AnalyzerBindingOwner::Field
                    if previous.index.name.is_some() || previous.search.name.is_some() =>
                {
                    return Err(SQLError::Unsupported(
                        "GIN analyzer option competes with an existing field assignment".into(),
                    ))
                }
                uqa_storage::AnalyzerBindingOwner::Gin
                    if previous.index.name.as_deref() != Some(name)
                        || previous.index.compiled.descriptor().fingerprint()
                            != revision.descriptor().fingerprint() =>
                {
                    return Err(SQLError::Unsupported(
                        "GIN analyzer option competes with an existing GIN revision".into(),
                    ))
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
            self.current_field_analyzer_binding(&table_name, &t, &field)
                .map_err(|error| storage_error("resolve FTS analyzer binding", &error))?
        };
        {
            let mut fts = t.fts_fields.write();
            if !fts.contains(&field) {
                fts.push(field.clone());
            }
        }
        let mut source = Self::fts_source(&t, Some(&self.runtime.cancellation))
            .map_err(|error| storage_error("read FTS rebuild source", &error))?;
        let phase = if analyzer.is_some() {
            AnalyzerPhase::Both
        } else {
            AnalyzerPhase::Index
        };
        t.inverted_index
            .write()
            .rebuild_with_analyzer_revision_cancellable(
                &field,
                candidate.index.compiled.clone(),
                phase,
                &mut source,
                &self.runtime.cancellation,
            )
            .map_err(|error| storage_error("add_fts_field", &error))?;
        candidate
            .install(&field, t.inverted_index.write().as_mut())
            .map_err(|error| storage_error("install FTS analyzer", &error))?;
        if uses_default {
            *t.analyzer.write() = candidate
                .index
                .compiled
                .descriptor()
                .configuration()
                .map_err(|error| storage_error("read FTS analyzer configuration", &error.into()))?;
        }
        self.persist_field_analyzer_binding(&table_name, &field, &candidate)
            .map_err(|error| storage_error("persist FTS analyzer binding", &error))?;
        self.durable
            .table_field_analyzers
            .write()
            .insert((table_name.clone(), field), candidate);
        if self.is_persistent() {
            self.try_save_table_schema(&table_name, &t)
                .map_err(|error| storage_error("persist FTS schema", &error))?;
        }
        Ok(())
    }

    /// Remove a field from the physical FTS index and from every piece of
    /// analyzer/schema metadata that makes the field searchable.  Callers
    /// must first establish that no other logical GIN index still references
    /// the field.
    pub(crate) fn drop_fts_field(&self, table: &str, field: &str) -> Result<(), SQLError> {
        self.with_implicit_transaction(|engine| engine.drop_fts_field_inner(table, field))
    }

    pub(super) fn drop_fts_field_inner(&self, table: &str, field: &str) -> Result<(), SQLError> {
        let table_name = self
            .try_resolve_table_name(table)
            .map_err(|error| storage_error("resolve FTS table", &error))?
            .ok_or_else(|| SQLError::UnknownTable(table.into()))?;
        let t = self
            .try_table(&table_name)
            .map_err(|error| storage_error("resolve FTS table", &error))?
            .ok_or_else(|| SQLError::UnknownTable(table_name.clone()))?;

        if !t
            .fts_fields
            .read()
            .iter()
            .any(|candidate| candidate == field)
        {
            return Err(SQLError::Internal(format!(
                "field `{table_name}`.`{field}` is not registered in the physical FTS index"
            )));
        }

        t.fts_fields.write().retain(|candidate| candidate != field);
        Self::rebuild_fts_index_cancellable(&t, &self.runtime.cancellation)
            .map_err(|error| storage_error("rebuild FTS index", &error))?;
        t.inverted_index
            .write()
            .remove_field_analyzers(field)
            .map_err(|error| SQLError::Internal(format!("remove FTS analyzer: {error}")))?;

        if let Some(catalog) = self.storage.catalog.as_ref() {
            catalog
                .drop_table_field_analyzer_field(&table_name, field)
                .map_err(|error| storage_error("drop persisted FTS analyzer", &error))?;
        }
        self.durable
            .table_field_analyzers
            .write()
            .remove(&(table_name.clone(), field.to_string()));
        if self.is_persistent() {
            self.try_save_table_schema(&table_name, &t)
                .map_err(|error| storage_error("persist FTS schema", &error))?;
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
    ) -> Result<(), SQLError> {
        let table_name = self
            .try_resolve_table_name(table)
            .map_err(|error| storage_error("resolve FTS table", &error))?
            .ok_or_else(|| SQLError::UnknownTable(table.into()))?;
        let t = self
            .try_table(&table_name)
            .map_err(|error| storage_error("resolve FTS table", &error))?
            .ok_or_else(|| SQLError::UnknownTable(table.into()))?;
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
            .map_err(|error| storage_error("compile FTS analyzer", &error.into()))?;
        let binding =
            uqa_storage::FieldAnalyzerBinding::unassigned(revision.clone(), revision.clone());
        let mut source = Self::fts_source(&t, Some(&self.runtime.cancellation))
            .map_err(|error| storage_error("read FTS rebuild source", &error))?;
        t.inverted_index
            .write()
            .rebuild_with_analyzer_revision_cancellable(
                field,
                revision.clone(),
                AnalyzerPhase::Both,
                &mut source,
                &self.runtime.cancellation,
            )
            .map_err(|error| storage_error("rebuild FTS analyzer", &error))?;
        *t.analyzer.write() = revision
            .descriptor()
            .configuration()
            .map_err(|error| storage_error("read FTS analyzer configuration", &error.into()))?;
        self.persist_field_analyzer_binding(&table_name, field, &binding)
            .map_err(|error| storage_error("persist FTS analyzer binding", &error))?;
        self.durable
            .table_field_analyzers
            .write()
            .insert((table_name.clone(), field.to_owned()), binding);
        if self.is_persistent() && t.persistence != uqa_sql::ast::RelationPersistence::Temporary {
            self.try_save_table_schema(&table_name, &t)
                .map_err(|error| storage_error("persist FTS schema", &error))?;
            self.note_table_catalog_changed();
            self.note_catalog_registry_changed();
        }
        Ok(())
    }
}
