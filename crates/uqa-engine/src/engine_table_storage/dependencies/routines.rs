//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Generated-column dependencies on durable routine identities.

use crate::engine_open::CatalogRestoreMode;
use crate::{Engine, StorageBackendError, StorageBackendResult};

use super::super::walk_schema_expr_mut;

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
    pub(crate) fn restore_generated_routine_identities(
        &self,
        mode: CatalogRestoreMode,
    ) -> StorageBackendResult<()> {
        let mut updates = Vec::new();
        for (table_name, table) in self.table_entries() {
            let mut columns = table.columns.read().clone();
            let mut requires_migration = false;
            for column in &columns {
                let Some(generated) = column.generated.as_ref() else {
                    continue;
                };
                if generated.function_dependencies.iter().any(|binding| {
                    !binding.builtin
                        && binding.dispatch.is_none()
                        && binding.resolution_error.is_none()
                        && binding.object_id.is_none()
                }) || schema_expr_has_legacy_routine_identity(&generated.expression)?
                {
                    requires_migration = true;
                    break;
                }
            }
            if !requires_migration {
                continue;
            }
            if !mode.allows_migration() {
                return Err(StorageBackendError::Other(format!(
                    "generated columns on `{table_name}` require an initial-open routine-identity migration"
                )));
            }
            let key_constraints = table.key_constraints.read().clone();
            let foreign_keys = table.foreign_keys.read().clone();
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
            if self.is_persistent() {
                self.try_save_table_schema_with_columns(&table_name, &table, &columns)?;
            }
            updates.push((table, columns));
        }
        for (table, columns) in updates {
            *table.columns.write() = columns;
        }
        Ok(())
    }

    pub(crate) fn rewrite_generated_routine_identity(
        &self,
        target: &uqa_sql::ast::FunctionBinding,
        new_name: &str,
    ) -> StorageBackendResult<()> {
        let mut updates = Vec::new();
        for (table_name, table) in self.table_entries() {
            let mut columns = table.columns.read().clone();
            let mut changed = false;
            for column in &mut columns {
                let Some(generated) = column.generated.as_mut() else {
                    continue;
                };
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
            if !changed {
                continue;
            }
            if self.is_persistent() {
                self.try_save_table_schema_with_columns(&table_name, &table, &columns)?;
            }
            updates.push((table, columns));
        }
        let changed = !updates.is_empty();
        for (table, columns) in updates {
            *table.columns.write() = columns;
        }
        if changed {
            self.note_table_catalog_changed();
        }
        Ok(())
    }
}
