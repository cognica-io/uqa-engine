//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Canonical SQL definition and FDW projection for a foreign table.

use crate::{CatalogFacade, Engine, RelationIdentity, StorageBackendError, StorageBackendResult};

pub(crate) use uqa_execution::catalog::foreign::StoredForeignTable;

impl Engine {
    pub(crate) fn migrate_foreign_table_identities(
        catalog: &dyn CatalogFacade,
    ) -> StorageBackendResult<()> {
        for mut row in catalog.load_foreign_tables()? {
            let relation_name = row.relation.qualified_name();
            let options = serde_json::from_str(&row.options_json)?;
            let (mut table, legacy_schema) = StoredForeignTable::from_catalog(
                relation_name,
                row.server_name.clone(),
                options,
                &row.columns_json,
            )?;
            let mut changed = legacy_schema;
            if table.object_id == [0; 16] {
                table.object_id = crate::new_table_object_id()?;
                changed = true;
            }
            let mut constraints = uqa_sql::ast::TableConstraintSet {
                checks: std::mem::take(&mut table.checks),
                ..uqa_sql::ast::TableConstraintSet::default()
            };
            changed |= crate::table_storage::materialize_constraint_metadata(
                &row.relation,
                &mut table.columns,
                &mut constraints,
            )?;
            table.checks = constraints.checks;
            changed |= Self::materialize_persisted_foreign_implicit_sequences(
                catalog,
                &row.relation,
                &row.role_owner,
                table.object_id,
                &mut table.columns,
            )?;
            if changed {
                row.columns_json = table.schema_json()?;
                catalog.save_foreign_table(&row)?;
            }
        }
        Ok(())
    }

    pub(crate) fn persist_foreign_table_definition(
        &self,
        relation: &RelationIdentity,
        table: &StoredForeignTable,
    ) -> StorageBackendResult<()> {
        let Some(catalog) = self.storage.catalog.as_ref() else {
            return Ok(());
        };
        let security = self
            .durable
            .foreign_table_security
            .read()
            .get(relation)
            .cloned()
            .ok_or_else(|| {
                StorageBackendError::Other(format!(
                    "foreign table `{}` has no security metadata",
                    relation.qualified_name()
                ))
            })?;
        catalog.save_foreign_table(&table.catalog_row(relation, &security)?)
    }

    fn update_foreign_table_definition(
        &self,
        table_name: &str,
        update: impl FnOnce(&mut StoredForeignTable) -> StorageBackendResult<bool>,
    ) -> StorageBackendResult<Option<bool>> {
        let relation =
            RelationIdentity::from_legacy_name(table_name).map_err(StorageBackendError::Other)?;
        let Some(mut table) = self.durable.foreign_tables.read().get(&relation).cloned() else {
            return Ok(None);
        };
        if !update(&mut table)? {
            return Ok(Some(false));
        }
        self.persist_foreign_table_definition(&relation, &table)?;
        self.durable.foreign_tables.write().insert(relation, table);
        self.note_catalog_registry_changed();
        Ok(Some(true))
    }

    pub(crate) fn clear_foreign_table_default_dependency(
        &self,
        table_name: &str,
        column_name: &str,
    ) -> StorageBackendResult<Option<bool>> {
        self.update_foreign_table_definition(table_name, |table| {
            let Some(column) = table
                .columns
                .iter_mut()
                .find(|column| column.name == column_name)
            else {
                return Ok(false);
            };
            Ok(column.default.take().is_some())
        })
    }

    pub(crate) fn drop_foreign_table_check_dependency(
        &self,
        table_name: &str,
        constraint_name: &str,
    ) -> StorageBackendResult<Option<bool>> {
        self.update_foreign_table_definition(table_name, |table| {
            for column in &mut table.columns {
                if column.check.is_some() && column.check_name.as_deref() == Some(constraint_name) {
                    column.check = None;
                    column.check_name = None;
                    column.check_object_id = None;
                    column.check_is_local = true;
                    column.check_enforced = true;
                    column.check_validated = true;
                    column.check_no_inherit = false;
                    return Ok(true);
                }
            }
            let Some(index) = table
                .checks
                .iter()
                .position(|check| check.name.as_deref() == Some(constraint_name))
            else {
                return Ok(false);
            };
            table.checks.remove(index);
            Ok(true)
        })
    }

    pub(crate) fn drop_foreign_table_generated_column_dependency(
        &self,
        table_name: &str,
        column_name: &str,
    ) -> StorageBackendResult<Option<bool>> {
        self.drop_foreign_table_column_dependency(table_name, column_name)
    }

    pub(crate) fn drop_foreign_table_column_dependency(
        &self,
        table_name: &str,
        column_name: &str,
    ) -> StorageBackendResult<Option<bool>> {
        let relation =
            RelationIdentity::from_legacy_name(table_name).map_err(StorageBackendError::Other)?;
        let Some(mut table) = self.durable.foreign_tables.read().get(&relation).cloned() else {
            return Ok(None);
        };
        let Some(column_index) = table
            .columns
            .iter()
            .position(|column| column.name == column_name)
        else {
            return Ok(Some(false));
        };
        let dependent_views = self.views_depending_on_column(table_name, column_name)?;
        if !dependent_views.is_empty() {
            return Err(StorageBackendError::Other(format!(
                "cannot drop generated column `{table_name}`.`{column_name}` while dependent view(s) `{}` remain",
                dependent_views.join("`, `")
            )));
        }
        for column in &table.columns {
            if column.name == column_name {
                continue;
            }
            if column.default.as_ref().is_some_and(|expression| {
                crate::table_storage::schema_expr_references_column(expression, column_name)
            }) || column.generated.as_ref().is_some_and(|generated| {
                crate::table_storage::schema_expr_references_column(
                    &generated.expression,
                    column_name,
                )
            }) {
                return Err(StorageBackendError::Other(format!(
                    "cannot drop generated column `{table_name}`.`{column_name}` because column `{}` depends on it",
                    column.name
                )));
            }
        }
        self.event_lifecycle_context()
            .handle_drop_column_event_dependencies(table_name, column_name, false)
            .map_err(|error| StorageBackendError::Other(error.to_string()))?;
        for column in &mut table.columns {
            if column.name != column_name
                && column.check.as_ref().is_some_and(|expression| {
                    crate::table_storage::schema_expr_references_column(expression, column_name)
                })
            {
                column.check = None;
                column.check_name = None;
                column.check_object_id = None;
                column.check_is_local = true;
                column.check_enforced = true;
                column.check_validated = true;
                column.check_no_inherit = false;
            }
        }
        table.columns.remove(column_index);
        table.checks.retain(|check| {
            !crate::table_storage::schema_expr_references_column(&check.expr, column_name)
        });
        let mut security = self
            .durable
            .foreign_table_security
            .read()
            .get(&relation)
            .cloned()
            .ok_or_else(|| {
                StorageBackendError::Other(format!(
                    "foreign table `{table_name}` has no security metadata"
                ))
            })?;
        security.column_acls.remove(column_name);
        if let Some(catalog) = self.storage.catalog.as_ref() {
            catalog.save_foreign_table(&table.catalog_row(&relation, &security)?)?;
        }
        self.durable
            .foreign_tables
            .write()
            .insert(relation.clone(), table);
        self.durable
            .foreign_table_security
            .write()
            .insert(relation, security);
        self.note_catalog_registry_changed();
        Ok(Some(true))
    }

    pub(crate) fn detach_foreign_table_sequence_provenance(
        &self,
        sequence: &str,
    ) -> StorageBackendResult<bool> {
        let mut updates = Vec::new();
        for (relation, mut table) in self.durable.foreign_tables.read().clone() {
            let mut changed = false;
            for column in &mut table.columns {
                if column
                    .auto_increment
                    .as_ref()
                    .is_some_and(|provenance| provenance.sequence.as_deref() == Some(sequence))
                {
                    column.auto_increment = None;
                    changed = true;
                }
            }
            if changed {
                updates.push((relation, table));
            }
        }
        for (relation, table) in &updates {
            self.persist_foreign_table_definition(relation, table)?;
        }
        let changed = !updates.is_empty();
        if changed {
            let mut tables = self.durable.foreign_tables.write();
            for (relation, table) in updates {
                tables.insert(relation, table);
            }
            drop(tables);
            self.note_catalog_registry_changed();
        }
        Ok(changed)
    }
}
