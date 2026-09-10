//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Schema-expression dependencies on durable routine identities.

use crate::open::CatalogRestoreMode;
use crate::{Engine, StorageBackendError, StorageBackendResult};

fn rewrite_schema_routine_references(
    columns: &mut [uqa_sql::ast::ColumnDef],
    checks: &mut [uqa_sql::ast::TableCheck],
    target: &uqa_sql::ast::FunctionBinding,
    new_name: &str,
) -> StorageBackendResult<bool> {
    uqa_sql::schema::dependencies::registration::rewrite_schema_routine_references(
        columns, checks, target, new_name,
    )
    .map_err(StorageBackendError::Other)
}
fn schema_expr_has_legacy_routine_identity(
    expression: &uqa_sql::ast::Expr,
) -> StorageBackendResult<bool> {
    uqa_sql::schema::dependencies::registration::schema_expr_has_legacy_routine_identity(expression)
        .map_err(StorageBackendError::Other)
}

impl Engine {
    pub(crate) fn restore_schema_routine_identities(
        &self,
        mode: CatalogRestoreMode,
    ) -> StorageBackendResult<()> {
        let mut updates = Vec::new();
        for (table_name, table) in self.table_entries() {
            let mut columns = table.columns.read().clone();
            let mut checks = table.table_checks.read().clone();
            let mut generated_requires_migration = false;
            for column in &columns {
                if let Some(generated) = column.generated.as_ref() {
                    generated_requires_migration |=
                        generated.function_dependencies.iter().any(|binding| {
                            !binding.builtin
                                && binding.dispatch.is_none()
                                && binding.resolution_error.is_none()
                                && binding.object_id.is_none()
                        }) || schema_expr_has_legacy_routine_identity(&generated.expression)?;
                }
            }
            let relation_requires_migration =
                self.bind_table_schema_regclass_constants(&mut columns, &mut checks, true)?;
            let expression_requires_migration = relation_requires_migration
                | self.bind_table_schema_routine_identities(
                    &table_name,
                    &mut columns,
                    &mut checks,
                )?;
            if !generated_requires_migration && !expression_requires_migration {
                continue;
            }
            if !mode.allows_migration() {
                return Err(StorageBackendError::Other(format!(
                    "schema expressions on `{table_name}` require an initial-open routine-identity migration"
                )));
            }
            let key_constraints = table.key_constraints.read().clone();
            let foreign_keys = table.foreign_keys.read().clone();
            let hierarchy = table.hierarchy.read().clone();
            if generated_requires_migration {
                crate::sql::prepare_generated_columns(
                    self,
                    &table_name,
                    &mut columns,
                    &key_constraints,
                    &foreign_keys,
                )
                .map_err(|error| {
                    StorageBackendError::Other(format!(
                        "migrate generated-column routine identities for `{table_name}`: {error}"
                    ))
                })?;
            }
            if self.is_persistent() {
                self.persist_constraint_candidate_with_hierarchy(
                    &table_name,
                    &table,
                    &columns,
                    &checks,
                    &foreign_keys,
                    &key_constraints,
                    &hierarchy,
                )?;
            }
            updates.push((table, columns, checks));
        }
        for (table, columns, checks) in updates {
            *table.columns.write() = columns;
            *table.table_checks.write() = checks;
        }
        Ok(())
    }

    pub(crate) fn bind_table_schema_routine_identities(
        &self,
        table_name: &str,
        columns: &mut [uqa_sql::ast::ColumnDef],
        checks: &mut [uqa_sql::ast::TableCheck],
    ) -> StorageBackendResult<bool> {
        uqa_sql::schema::dependencies::registration::bind_table_schema_routine_identities(
            &self.schema_dependency_binding_context(),
            table_name,
            columns,
            checks,
        )
        .map_err(StorageBackendError::Other)
    }
    pub(in crate::table_storage) fn bind_table_schema_routine_identities_with_check_columns(
        &self,
        table_name: &str,
        columns: &mut [uqa_sql::ast::ColumnDef],
        checks: &mut [uqa_sql::ast::TableCheck],
        check_columns: &[uqa_sql::ast::ColumnDef],
    ) -> StorageBackendResult<bool> {
        uqa_sql::schema::dependencies::registration::bind_table_schema_routine_identities_with_check_columns(&self.schema_dependency_binding_context(), table_name, columns, checks, check_columns).map_err(StorageBackendError::Other)
    }
    pub(crate) fn bind_default_routine_identities(
        &self,
        table_name: &str,
        column_name: &str,
        default: &mut uqa_sql::ast::Expr,
    ) -> StorageBackendResult<bool> {
        uqa_sql::schema::dependencies::registration::bind_default_routine_identities(
            &self.schema_dependency_binding_context(),
            table_name,
            column_name,
            default,
        )
        .map_err(StorageBackendError::Other)
    }

    pub(crate) fn rewrite_schema_routine_identity(
        &self,
        target: &uqa_sql::ast::FunctionBinding,
        new_name: &str,
    ) -> StorageBackendResult<()> {
        self.rewrite_index_routine_identity(target, new_name)?;
        let mut table_updates = Vec::new();
        for (table_name, table) in self.table_entries() {
            let mut columns = table.columns.read().clone();
            let mut checks = table.table_checks.read().clone();
            if rewrite_schema_routine_references(&mut columns, &mut checks, target, new_name)? {
                table_updates.push((table_name, table, columns, checks));
            }
        }

        let mut foreign_updates = Vec::new();
        for (relation, mut table) in self.durable.foreign_tables.read().clone() {
            if rewrite_schema_routine_references(
                &mut table.columns,
                &mut table.checks,
                target,
                new_name,
            )? {
                foreign_updates.push((relation, table));
            }
        }

        if self.is_persistent() {
            for (table_name, table, columns, checks) in &table_updates {
                self.persist_constraint_candidate(
                    table_name,
                    table,
                    columns,
                    checks,
                    &table.foreign_keys.read(),
                    &table.key_constraints.read(),
                )?;
            }
            for (relation, table) in &foreign_updates {
                self.persist_foreign_table_definition(relation, table)?;
            }
        }

        let tables_changed = !table_updates.is_empty();
        for (_, table, columns, checks) in table_updates {
            *table.columns.write() = columns;
            *table.table_checks.write() = checks;
        }
        let foreign_tables_changed = !foreign_updates.is_empty();
        if foreign_tables_changed {
            let mut tables = self.durable.foreign_tables.write();
            for (relation, table) in foreign_updates {
                tables.insert(relation, table);
            }
        }
        if tables_changed {
            self.note_table_catalog_changed();
        }
        if foreign_tables_changed {
            self.note_catalog_registry_changed();
        }
        Ok(())
    }
}
