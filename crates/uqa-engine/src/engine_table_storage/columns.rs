//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Column registration, index rebuild, and table or column rename.

use super::{
    materialize_constraint_metadata, schema_expr_references_column, table_next_id_metadata_key,
    table_not_found, Engine, RelationIdentity, StorageBackendError, StorageBackendResult,
};
use crate::VectorIndexSpec;

impl Engine {
    /// Append a column to the schema. No data migration is needed because
    /// the document store is sparse; rows missing the column read back as
    /// `Value::Null`.
    pub fn register_column(
        &self,
        table: &str,
        column: uqa_sql::ast::ColumnDef,
    ) -> StorageBackendResult<()> {
        self.try_register_column(table, column)
    }

    pub(crate) fn try_register_column(
        &self,
        table: &str,
        column: uqa_sql::ast::ColumnDef,
    ) -> StorageBackendResult<()> {
        self.with_implicit_storage_transaction(|engine| {
            engine.try_register_column_inner(table, column, None)
        })
    }

    pub(crate) fn try_register_column_with_check_columns(
        &self,
        table: &str,
        column: uqa_sql::ast::ColumnDef,
        check_columns: &[uqa_sql::ast::ColumnDef],
    ) -> StorageBackendResult<()> {
        self.with_implicit_storage_transaction(|engine| {
            engine.try_register_column_inner(table, column, Some(check_columns))
        })
    }

    pub(super) fn try_register_column_inner(
        &self,
        table: &str,
        mut column: uqa_sql::ast::ColumnDef,
        check_columns: Option<&[uqa_sql::ast::ColumnDef]>,
    ) -> StorageBackendResult<()> {
        let legacy_auto_increment = column
            .auto_increment
            .as_ref()
            .is_some_and(uqa_sql::ast::AutoIncrement::is_legacy);
        let table_name = self
            .try_resolve_table_name(table)?
            .ok_or_else(|| table_not_found(table))?;
        let t = self
            .try_table(&table_name)?
            .ok_or_else(|| table_not_found(&table_name))?;
        if let Some(default) = &mut column.default {
            self.bind_sequence_references_in_expr(default)?;
        }
        if let Some(generated) = &mut column.generated {
            self.bind_sequence_references_in_expr(&mut generated.expression)?;
        }
        if let Some(reference) = &mut column.references {
            reference.table = self.canonical_foreign_key_target(&reference.table)?;
        }
        let mut columns = t.columns.read().clone();
        if columns.iter().any(|c| c.name == column.name) {
            return Err(StorageBackendError::Other(format!(
                "column `{}` already exists on table `{table_name}`",
                column.name
            )));
        }
        columns.push(column);
        if let Some(check_columns) = check_columns {
            self.bind_table_schema_routine_identities_with_check_columns(
                &table_name,
                &mut columns,
                &mut [],
                check_columns,
            )?;
        } else {
            self.bind_table_schema_routine_identities(&table_name, &mut columns, &mut [])?;
        }
        let mut constraints = uqa_sql::ast::TableConstraintSet {
            persistence: t.persistence,
            on_commit: t.on_commit,
            checks: t.table_checks.read().clone(),
            foreign_keys: t.foreign_keys.read().clone(),
            key_constraints: t.key_constraints.read().clone(),
            hierarchy: t.hierarchy.read().clone(),
        };
        let relation =
            RelationIdentity::from_legacy_name(&table_name).map_err(StorageBackendError::Other)?;
        materialize_constraint_metadata(&relation, &mut columns, &mut constraints)?;
        self.mark_column_stats_dirty(&table_name, &t)?;
        if self.is_persistent() {
            self.try_save_table_schema_with_components(&table_name, &t, &columns, &constraints)?;
        }
        *t.columns.write() = columns;
        *t.table_checks.write() = constraints.checks;
        *t.foreign_keys.write() = constraints.foreign_keys;
        *t.key_constraints.write() = constraints.key_constraints;
        if legacy_auto_increment {
            self.persist_next_id(&table_name)?;
        }
        self.refresh_value_indexes_for_table(&table_name)?;
        Ok(())
    }

    pub fn drop_column(&self, table: &str, column: &str) -> StorageBackendResult<bool> {
        self.try_drop_column(table, column)
    }

    pub(crate) fn try_drop_column(&self, table: &str, column: &str) -> StorageBackendResult<bool> {
        self.with_implicit_storage_transaction(|engine| engine.try_drop_column_inner(table, column))
    }

    pub(crate) fn try_drop_column_cascade(
        &self,
        table: &str,
        column: &str,
    ) -> StorageBackendResult<bool> {
        self.with_implicit_storage_transaction(|engine| {
            engine.try_drop_column_inner_with_sequence_cascade(table, column, true)
        })
    }

    pub(crate) fn try_drop_column_inner(
        &self,
        table: &str,
        column: &str,
    ) -> StorageBackendResult<bool> {
        self.try_drop_column_inner_with_sequence_cascade(table, column, false)
    }

    fn try_drop_column_inner_with_sequence_cascade(
        &self,
        table: &str,
        column: &str,
        cascade: bool,
    ) -> StorageBackendResult<bool> {
        let Some(table_name) = self.resolve_table_ddl_target(table, "ALTER TABLE DROP COLUMN")?
        else {
            return Ok(false);
        };
        let Some(t) = self.try_table(table)? else {
            return Ok(false);
        };
        if !t
            .columns
            .read()
            .iter()
            .any(|candidate| candidate.name == column)
        {
            return Ok(false);
        }
        let column_object_id = t
            .columns
            .read()
            .iter()
            .find(|candidate| candidate.name == column)
            .and_then(|candidate| candidate.object_id)
            .ok_or_else(|| {
                StorageBackendError::Other(format!(
                    "column `{table_name}`.`{column}` has no object identity"
                ))
            })?;
        let owned_sequences =
            self.sequence_names_owned_by_column(t.object_id(), column_object_id)?;
        self.preflight_drop_column_dependencies(&table_name, column)?;
        let prepared_rule_drop = self.prepare_rule_column_drop(&table_name, column)?;
        Self::value_indexes_clear(&t);
        {
            let mut cols = t.columns.write();
            for definition in cols.iter_mut() {
                if definition
                    .check
                    .as_ref()
                    .is_some_and(|expression| schema_expr_references_column(expression, column))
                {
                    definition.check = None;
                    definition.check_name = None;
                    definition.check_enforced = true;
                    definition.check_validated = true;
                    definition.check_no_inherit = false;
                }
            }
            cols.retain(|c| c.name != column);
        }
        t.table_checks
            .write()
            .retain(|constraint| !schema_expr_references_column(&constraint.expr, column));
        t.key_constraints
            .write()
            .retain(|constraint| !constraint.columns.iter().any(|name| name == column));
        t.foreign_keys.write().retain(|foreign_key| {
            !foreign_key.local_columns.iter().any(|name| name == column)
                && !foreign_key
                    .on_delete_set_columns
                    .iter()
                    .any(|name| name == column)
        });
        t.security.write().column_acls.remove(column);
        // Remove from FTS field list if present.
        {
            let mut fts = t.fts_fields.write();
            fts.retain(|f| f != column);
        }
        // Drop the vector index for this field if it exists.
        {
            let mut vs = t.vector_indexes.write();
            if let Some(mut idx) = vs.remove(column) {
                idx.clear()?;
            }
        }
        self.remove_catalog_indexes_for_column(&table_name, column)?;
        self.durable
            .table_field_analyzers
            .write()
            .retain(|(table, field), _| !(table == &table_name && field == column));
        let ids = t.document_store.read().doc_ids()?;
        for doc_id in ids {
            let Some(mut doc) = t.document_store.read().get(doc_id)? else {
                continue;
            };
            if doc.remove(column).is_some() {
                self.rewrite_document_for_schema_change(&table_name, doc_id, doc)
                    .map_err(|err| StorageBackendError::Other(err.to_string()))?;
            }
        }
        self.persist_dropped_column(&table_name, column, &t)?;
        self.finish_rule_column_drop(prepared_rule_drop)?;
        for sequence in owned_sequences {
            self.drop_owned_sequence(&sequence, cascade)?;
        }
        self.mark_column_stats_dirty(&table_name, &t)?;
        self.refresh_value_indexes_for_table(&table_name)?;
        self.prune_constraint_modes()
            .map_err(|error| StorageBackendError::Other(error.to_string()))?;
        Ok(true)
    }

    fn persist_dropped_column(
        &self,
        table_name: &str,
        column: &str,
        table: &super::TableState,
    ) -> StorageBackendResult<()> {
        if !self.is_persistent() {
            return Ok(());
        }
        if let Some(catalog) = self.storage.catalog.as_ref() {
            catalog.drop_column_data(table_name, column)?;
        }
        self.try_save_table_schema(table_name, table)
    }

    pub(crate) fn try_drop_vector_indexes_for_column(
        &self,
        table: &str,
        column: &str,
    ) -> StorageBackendResult<bool> {
        self.with_implicit_storage_transaction(|engine| {
            engine.try_drop_vector_indexes_for_column_inner(table, column)
        })
    }

    pub(super) fn try_drop_vector_indexes_for_column_inner(
        &self,
        table: &str,
        column: &str,
    ) -> StorageBackendResult<bool> {
        let Some(table_name) = self.resolve_table_ddl_target(table, "ALTER TABLE ALTER COLUMN")?
        else {
            return Ok(false);
        };
        let Some(t) = self.try_table(table)? else {
            return Ok(false);
        };
        if let Some(mut idx) = t.vector_indexes.write().remove(column) {
            idx.clear()?;
        }
        for index_name in self.vector_catalog_index_names_for_column(&table_name, column)? {
            self.try_drop_catalog_index(&index_name)?;
        }
        self.try_save_table_schema(&table_name, &t)?;
        Ok(true)
    }

    pub(crate) fn try_rebuild_vector_index_for_column(
        &self,
        table: &str,
        column: &str,
        dimensions: u32,
    ) -> StorageBackendResult<bool> {
        self.with_implicit_storage_transaction(|engine| {
            engine.try_rebuild_vector_index_for_column_inner(table, column, dimensions)
        })
    }

    pub(super) fn try_rebuild_vector_index_for_column_inner(
        &self,
        table: &str,
        column: &str,
        dimensions: u32,
    ) -> StorageBackendResult<bool> {
        let table_name = self
            .try_resolve_table_name(table)?
            .ok_or_else(|| table_not_found(table))?;
        let spec = self
            .vector_index_spec_for_column(&table_name, column)?
            .unwrap_or(VectorIndexSpec::BruteForce);
        let rebuilt = self.rebuild_vector_field_with_spec(&table_name, column, dimensions, spec)?;
        if !rebuilt {
            return Err(StorageBackendError::Other(format!(
                "failed to rebuild vector index for `{table_name}`.`{column}`"
            )));
        }
        let t = self
            .try_table(&table_name)?
            .ok_or_else(|| table_not_found(&table_name))?;
        self.try_save_table_schema(&table_name, &t)?;
        Ok(true)
    }

    pub fn rename_column(&self, table: &str, from: &str, to: &str) -> StorageBackendResult<bool> {
        self.try_rename_column(table, from, to)
    }

    pub(crate) fn try_rename_column(
        &self,
        table: &str,
        from: &str,
        to: &str,
    ) -> StorageBackendResult<bool> {
        self.with_implicit_storage_transaction(|engine| {
            engine.try_rename_column_inner(table, from, to)
        })
    }

    fn rename_column_acl(table: &super::TableState, from: &str, to: &str) {
        if from == to {
            return;
        }
        let mut security = table.security.write();
        if let Some(acl) = security.column_acls.remove(from) {
            security.column_acls.insert(to.to_string(), acl);
        }
    }

    pub(super) fn try_rename_column_inner(
        &self,
        table: &str,
        from: &str,
        to: &str,
    ) -> StorageBackendResult<bool> {
        let Some(table_name) = self.resolve_table_ddl_target(table, "ALTER TABLE RENAME COLUMN")?
        else {
            return Ok(false);
        };
        let Some(t) = self.try_table(table)? else {
            return Ok(false);
        };
        {
            let columns = t.columns.read();
            if !columns.iter().any(|candidate| candidate.name == from) {
                return Ok(false);
            }
            if from != to && columns.iter().any(|candidate| candidate.name == to) {
                return Ok(false);
            }
        }
        self.rewrite_column_rename_dependencies(&table_name, from, to)?;
        Self::value_indexes_clear(&t);
        {
            let mut cols = t.columns.write();
            for c in cols.iter_mut() {
                if c.name == from {
                    c.name = to.to_string();
                }
            }
        }
        Self::rename_column_acl(&t, from, to);
        for constraint in t.key_constraints.write().iter_mut() {
            for column in &mut constraint.columns {
                if column == from {
                    *column = to.to_string();
                }
            }
        }
        {
            let mut fts = t.fts_fields.write();
            for f in fts.iter_mut() {
                if f == from {
                    *f = to.to_string();
                }
            }
        }
        let vector_dimensions = {
            let mut vs = t.vector_indexes.write();
            if let Some(mut idx) = vs.remove(from) {
                let dimensions = idx.dimensions();
                idx.clear()?;
                Some(dimensions)
            } else {
                None
            }
        };
        let ids = t.document_store.read().doc_ids()?;
        for doc_id in ids {
            let Some(mut doc) = t.document_store.read().get(doc_id)? else {
                continue;
            };
            if let Some(value) = doc.remove(from) {
                doc.insert(to.to_string(), value);
                self.rewrite_document_for_schema_change(&table_name, doc_id, doc)
                    .map_err(|err| StorageBackendError::Other(err.to_string()))?;
            }
        }
        if let Some(dimensions) = vector_dimensions {
            self.create_vector_field(&table_name, to, dimensions)?;
        }
        self.rename_catalog_index_column_refs(&table_name, from, to)?;
        {
            let mut analyzers = self.durable.table_field_analyzers.write();
            let mut moved = Vec::new();
            analyzers.retain(|(table, field), value| {
                if table == &table_name && field == from {
                    moved.push(((table_name.clone(), to.to_string()), value.clone()));
                    false
                } else {
                    true
                }
            });
            analyzers.extend(moved);
        }
        if self.is_persistent() {
            if let Some(catalog) = self.storage.catalog.as_ref() {
                catalog.rename_column_data(&table_name, from, to)?;
            }
            if let Some(dimensions) = vector_dimensions {
                if let Some(spec) = self.vector_index_spec_for_column(&table_name, to)? {
                    if !self.rebuild_vector_field_with_spec(&table_name, to, dimensions, spec)? {
                        return Err(StorageBackendError::Other(format!(
                            "failed to rebuild vector index for `{table_name}`.`{to}`"
                        )));
                    }
                }
            }
            self.try_save_table_schema(&table_name, &t)?;
        }
        self.rename_event_column_inner(&table_name, from, to)?;
        self.mark_column_stats_dirty(&table_name, &t)?;
        self.refresh_value_indexes_for_table(&table_name)?;
        Ok(true)
    }

    pub fn rename_table(&self, from: &str, to: &str) -> StorageBackendResult<bool> {
        self.try_rename_table(from, to)
    }

    pub(crate) fn try_rename_table(&self, from: &str, to: &str) -> StorageBackendResult<bool> {
        self.with_implicit_storage_transaction(|engine| engine.try_rename_table_inner(from, to))
    }

    pub(super) fn try_rename_table_inner(
        &self,
        from: &str,
        to: &str,
    ) -> StorageBackendResult<bool> {
        let Some(from) = self.resolve_table_ddl_target(from, "ALTER TABLE RENAME")? else {
            return Ok(false);
        };
        let from_relation = Self::resolved_relation_identity(&from)?;
        let (target_schema, target_name) =
            RelationIdentity::parse_reference(to).map_err(StorageBackendError::Other)?;
        let to_relation = RelationIdentity::new(
            target_schema.unwrap_or_else(|| from_relation.schema.clone()),
            target_name,
        );
        if to_relation.schema != self.temporary_schema_name()
            && !self
                .durable
                .schemas
                .read()
                .contains_key(&to_relation.schema)
        {
            return Err(StorageBackendError::Other(format!(
                "schema `{}` does not exist",
                to_relation.schema
            )));
        }
        let to = to_relation.qualified_name();
        if let Some(kind) = self.relation_kind_at(&to)? {
            return Err(StorageBackendError::Other(format!(
                "relation `{to}` already exists as {kind}"
            )));
        }
        let persist_catalog = {
            let tables = self.storage.tables.read();
            if !tables.contains_key(&from_relation) || tables.contains_key(&to_relation) {
                return Ok(false);
            }
            self.is_persistent()
                && tables.get(&from_relation).is_some_and(|table| {
                    table.persistence != uqa_sql::ast::RelationPersistence::Temporary
                })
        };
        self.rewrite_table_rename_dependencies(&from, &to)?;
        if persist_catalog {
            if let Some(catalog) = self.storage.catalog.as_ref() {
                catalog.rename_table_data(&from, &to)?;
            }
        }
        let mut tables = self.storage.tables.write();
        if tables.contains_key(&to_relation) {
            return Ok(false);
        }
        let Some(state) = tables.remove(&from_relation) else {
            return Ok(false);
        };
        tables.insert(to_relation.clone(), state.clone());
        drop(tables);
        self.rename_relation_events_inner(&from_relation, &to_relation)?;
        self.rename_catalog_index_table_refs(&from, &to);
        {
            let mut analyzers = self.durable.table_field_analyzers.write();
            let mut moved = Vec::new();
            analyzers.retain(|(table, field), value| {
                if table == &from {
                    moved.push(((to.clone(), field.clone()), value.clone()));
                    false
                } else {
                    true
                }
            });
            analyzers.extend(moved);
        }
        if persist_catalog {
            self.rebind_persistent_table_stores(&to, &state)?;
            self.try_save_table_schema(&to, &state)?;
            if state.columns.read().iter().any(|column| {
                column
                    .auto_increment
                    .as_ref()
                    .is_some_and(uqa_sql::ast::AutoIncrement::is_legacy)
            }) {
                self.persist_next_id(&to)?;
            }
            if let Some(catalog) = self.storage.catalog.as_ref() {
                catalog.set_metadata(&table_next_id_metadata_key(&from), "")?;
            }
        }
        self.row_locks.invalidate_column_stats(&from);
        self.mark_column_stats_dirty(&to, &state)?;
        self.refresh_value_indexes_for_table(&to)?;
        self.rename_constraint_transaction_relation(&from_relation, &to_relation);
        Ok(true)
    }
}
