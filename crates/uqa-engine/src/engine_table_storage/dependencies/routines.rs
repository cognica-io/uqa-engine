//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Schema-expression dependencies on durable routine identities.

use crate::engine_open::CatalogRestoreMode;
use crate::{Engine, StorageBackendError, StorageBackendResult};

use super::super::walk_schema_expr_mut;

fn schema_expr_may_require_routine_identity_binding(
    expression: &uqa_sql::ast::Expr,
) -> StorageBackendResult<bool> {
    let mut expression = expression.clone();
    let mut legacy = false;
    walk_schema_expr_mut(&mut expression, &mut |node| {
        if let uqa_sql::ast::Expr::Func { binding, .. } = node {
            legacy |= binding.as_ref().is_none_or(|binding| {
                !binding.builtin
                    && binding.dispatch.is_none()
                    && binding.resolution_error.is_none()
                    && binding.object_id.is_none()
            });
        }
        Ok(())
    })?;
    Ok(legacy)
}

fn schema_expr_has_legacy_routine_identity(
    expression: &uqa_sql::ast::Expr,
) -> StorageBackendResult<bool> {
    let mut expression = expression.clone();
    let mut legacy = false;
    walk_schema_expr_mut(&mut expression, &mut |node| {
        if let uqa_sql::ast::Expr::Func {
            binding: Some(binding),
            ..
        } = node
        {
            legacy |= !binding.builtin
                && binding.dispatch.is_none()
                && binding.resolution_error.is_none()
                && binding.object_id.is_none();
        }
        Ok(())
    })?;
    Ok(legacy)
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
            let expression_requires_migration =
                self.bind_table_schema_routine_identities(&table_name, &mut columns, &mut checks)?;
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
        let check_columns = columns.to_vec();
        self.bind_table_schema_routine_identities_with_check_columns(
            table_name,
            columns,
            checks,
            &check_columns,
        )
    }

    pub(in crate::engine_table_storage) fn bind_table_schema_routine_identities_with_check_columns(
        &self,
        table_name: &str,
        columns: &mut [uqa_sql::ast::ColumnDef],
        checks: &mut [uqa_sql::ast::TableCheck],
        check_columns: &[uqa_sql::ast::ColumnDef],
    ) -> StorageBackendResult<bool> {
        let mut changed = false;
        for column in columns {
            if let Some(default) = &mut column.default {
                changed |=
                    self.bind_default_routine_identities(table_name, &column.name, default)?;
            }
            if let Some(check) = &mut column.check {
                if schema_expr_may_require_routine_identity_binding(check)? {
                    changed |= crate::sql::bind_stored_check_expression_routines(
                        self,
                        table_name,
                        table_name,
                        check_columns,
                        check,
                    )
                    .map_err(|error| {
                        StorageBackendError::Other(format!(
                            "bind CHECK routine identities for `{table_name}`.`{}`: {error}",
                            column.name
                        ))
                    })?;
                }
            }
        }
        for check in checks {
            if schema_expr_may_require_routine_identity_binding(&check.expr)? {
                changed |= crate::sql::bind_stored_check_expression_routines(
                    self,
                    table_name,
                    table_name,
                    check_columns,
                    &mut check.expr,
                )
                .map_err(|error| {
                    StorageBackendError::Other(format!(
                        "bind CHECK routine identities for `{table_name}`: {error}"
                    ))
                })?;
            }
        }
        Ok(changed)
    }

    pub(crate) fn bind_default_routine_identities(
        &self,
        table_name: &str,
        column_name: &str,
        default: &mut uqa_sql::ast::Expr,
    ) -> StorageBackendResult<bool> {
        if !schema_expr_may_require_routine_identity_binding(default)? {
            return Ok(false);
        }
        crate::sql::bind_stored_schema_expression_routines(self, default, default.clone()).map_err(
            |error| {
                StorageBackendError::Other(format!(
                    "bind default routine identities for `{table_name}`.`{column_name}`: {error}"
                ))
            },
        )
    }

    pub(crate) fn rewrite_schema_routine_identity(
        &self,
        target: &uqa_sql::ast::FunctionBinding,
        new_name: &str,
    ) -> StorageBackendResult<()> {
        let mut updates = Vec::new();
        for (table_name, table) in self.table_entries() {
            let mut columns = table.columns.read().clone();
            let mut checks = table.table_checks.read().clone();
            let mut changed = false;
            for column in &mut columns {
                for expression in [&mut column.default, &mut column.check]
                    .into_iter()
                    .flatten()
                {
                    changed |= crate::engine_events::rewrite_expression_routine_identity(
                        expression, target, new_name,
                    )
                    .map_err(|error| StorageBackendError::Other(error.to_string()))?;
                }
                if let Some(generated) = column.generated.as_mut() {
                    changed |= crate::engine_events::rewrite_expression_routine_identity(
                        &mut generated.expression,
                        target,
                        new_name,
                    )
                    .map_err(|error| StorageBackendError::Other(error.to_string()))?;
                    for dependency in &mut generated.function_dependencies {
                        if crate::engine_session::function_binding_matches(dependency, target) {
                            dependency.name = new_name.to_string();
                            changed = true;
                        }
                    }
                }
            }
            for check in &mut checks {
                changed |= crate::engine_events::rewrite_expression_routine_identity(
                    &mut check.expr,
                    target,
                    new_name,
                )
                .map_err(|error| StorageBackendError::Other(error.to_string()))?;
            }
            if !changed {
                continue;
            }
            if self.is_persistent() {
                self.persist_constraint_candidate(
                    &table_name,
                    &table,
                    &columns,
                    &checks,
                    &table.foreign_keys.read(),
                    &table.key_constraints.read(),
                )?;
            }
            updates.push((table, columns, checks));
        }
        let changed = !updates.is_empty();
        for (table, columns, checks) in updates {
            *table.columns.write() = columns;
            *table.table_checks.write() = checks;
        }
        if changed {
            self.note_table_catalog_changed();
        }
        Ok(())
    }
}
