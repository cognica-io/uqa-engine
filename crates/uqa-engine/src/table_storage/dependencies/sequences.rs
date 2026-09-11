//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Sequence identities and lifecycle dependencies in stored schema expressions.

use crate::sequences::SequenceSchemaDependent;
use crate::{Engine, StorageBackendError, StorageBackendResult};

use super::super::rewrite_sequence_function_references;

fn expression_references_sequence(
    engine: &Engine,
    expression: Option<&uqa_sql::ast::Expr>,
    sequence: &str,
) -> StorageBackendResult<bool> {
    expression.map_or(Ok(false), |expression| {
        Ok(engine
            .stored_sequence_targets_in_loaded_expr(expression)?
            .contains(sequence))
    })
}

fn append_sequence_schema_expression_dependents(
    engine: &Engine,
    table_name: &str,
    columns: &[uqa_sql::ast::ColumnDef],
    checks: &[uqa_sql::ast::TableCheck],
    sequence: &str,
    foreign: bool,
    dependents: &mut Vec<SequenceSchemaDependent>,
) -> StorageBackendResult<()> {
    let relation = if foreign {
        format!("foreign table `{table_name}`")
    } else {
        format!("`{table_name}`")
    };
    for column in columns {
        if expression_references_sequence(engine, column.default.as_ref(), sequence)? {
            dependents.push(SequenceSchemaDependent::Default {
                table: table_name.to_string(),
                column: column.name.clone(),
                foreign,
            });
        }
        if expression_references_sequence(
            engine,
            column
                .generated
                .as_ref()
                .map(|generated| generated.expression.as_ref()),
            sequence,
        )? {
            dependents.push(SequenceSchemaDependent::GeneratedColumn {
                table: table_name.to_string(),
                column: column.name.clone(),
                foreign,
            });
        }
        if expression_references_sequence(engine, column.check.as_ref(), sequence)? {
            dependents.push(SequenceSchemaDependent::CheckConstraint {
                table: table_name.to_string(),
                constraint: column.check_name.clone().ok_or_else(|| {
                    StorageBackendError::Other(format!(
                        "CHECK constraint on {relation}.`{}` has no catalog name",
                        column.name
                    ))
                })?,
                foreign,
            });
        }
    }
    for check in checks {
        if expression_references_sequence(engine, Some(&check.expr), sequence)? {
            dependents.push(SequenceSchemaDependent::CheckConstraint {
                table: table_name.to_string(),
                constraint: check.name.clone().ok_or_else(|| {
                    StorageBackendError::Other(format!(
                        "table CHECK constraint on {relation} has no catalog name"
                    ))
                })?,
                foreign,
            });
        }
    }
    Ok(())
}

impl Engine {
    pub(in crate::table_storage) fn resolve_stored_sequence_references_in_expr(
        &self,
        expression: &mut uqa_sql::ast::Expr,
    ) -> StorageBackendResult<()> {
        let mut refreshed = false;
        rewrite_sequence_function_references(expression, &mut |reference| {
            if !refreshed {
                self.refresh_sequences_from_catalog()?;
                refreshed = true;
            }
            *reference = self.resolve_stored_sequence_reference_from_loaded_registry(reference)?;
            Ok(())
        })
    }

    fn stored_sequence_targets_in_loaded_expr(
        &self,
        expression: &uqa_sql::ast::Expr,
    ) -> StorageBackendResult<std::collections::BTreeSet<String>> {
        let mut expression = expression.clone();
        let mut targets = std::collections::BTreeSet::new();
        rewrite_sequence_function_references(&mut expression, &mut |reference| {
            let canonical =
                self.resolve_stored_sequence_reference_from_loaded_registry(reference)?;
            targets.insert(canonical.clone());
            *reference = canonical;
            Ok(())
        })?;
        let identities = self
            .durable
            .sequence_object_ids
            .read()
            .iter()
            .map(|(name, id)| {
                (
                    uqa_execution::catalog::projection::sequence_relation_oid(*id),
                    name.qualified_name(),
                )
            })
            .collect::<std::collections::BTreeMap<_, _>>();
        super::super::walk_schema_expr_mut(&mut expression, &mut |node| {
            if let Some(name) =
                super::regclass::regclass_constant_oid(node).and_then(|oid| identities.get(&oid))
            {
                targets.insert(name.clone());
            }
            Ok(())
        })?;
        Ok(targets)
    }

    pub(crate) fn sequence_schema_expression_dependents(
        &self,
        sequence: &str,
    ) -> StorageBackendResult<Vec<SequenceSchemaDependent>> {
        self.synchronize_table_catalog()?;
        self.synchronize_catalog_registries()?;
        self.refresh_sequences_from_catalog()?;
        let mut dependents = Vec::new();
        for (table_name, table) in self.table_entries() {
            append_sequence_schema_expression_dependents(
                self,
                &table_name,
                &table.columns.read(),
                &table.table_checks.read(),
                sequence,
                false,
                &mut dependents,
            )?;
        }
        for (relation, table) in self.durable.foreign_tables.read().iter() {
            let table_name = relation.qualified_name();
            append_sequence_schema_expression_dependents(
                self,
                &table_name,
                &table.columns,
                &table.checks,
                sequence,
                true,
                &mut dependents,
            )?;
        }
        dependents.sort();
        dependents.dedup();
        Ok(dependents)
    }

    pub(crate) fn ensure_no_sequence_schema_dependencies(
        &self,
        sequence: &str,
    ) -> StorageBackendResult<()> {
        let dependents = self.sequence_schema_expression_dependents(sequence)?;
        if dependents.is_empty() {
            return Ok(());
        }
        Err(StorageBackendError::Other(format!(
            "column schema expression(s) `{}` depend on sequence `{sequence}`",
            dependents
                .iter()
                .map(SequenceSchemaDependent::object_label)
                .collect::<Vec<_>>()
                .join("`, `")
        )))
    }

    fn drop_sequence_schema_dependencies(
        &self,
        dependencies: &[SequenceSchemaDependent],
    ) -> StorageBackendResult<()> {
        for dependency in dependencies {
            let SequenceSchemaDependent::CheckConstraint {
                table,
                constraint,
                foreign,
            } = dependency
            else {
                continue;
            };
            if *foreign {
                if self
                    .foreign_definition_context()
                    .drop_foreign_table_check_dependency(table, constraint)?
                    != Some(true)
                {
                    return Err(StorageBackendError::Other(format!(
                        "constraint `{constraint}` on foreign table `{table}` disappeared after sequence DROP preflight"
                    )));
                }
            } else {
                self.drop_constraint_dependency(table, constraint)
                    .map_err(|error| StorageBackendError::Other(error.to_string()))?;
            }
        }
        for dependency in dependencies {
            let SequenceSchemaDependent::Default {
                table,
                column,
                foreign,
            } = dependency
            else {
                continue;
            };
            let dropped = if *foreign {
                self.foreign_definition_context()
                    .clear_foreign_table_default_dependency(table, column)?
                    == Some(true)
            } else {
                self.set_column_default_inner(table, column, None)?
            };
            if !dropped {
                return Err(StorageBackendError::Other(format!(
                    "default `{table}`.`{column}` disappeared after sequence DROP preflight"
                )));
            }
        }
        for dependency in dependencies {
            let SequenceSchemaDependent::GeneratedColumn {
                table,
                column,
                foreign,
            } = dependency
            else {
                continue;
            };
            let dropped = if *foreign {
                self.foreign_definition_context()
                    .drop_foreign_table_column_dependency(table, column)?
                    == Some(true)
            } else {
                self.try_drop_column_inner(table, column)?
            };
            if !dropped {
                return Err(StorageBackendError::Other(format!(
                    "generated column `{table}`.`{column}` disappeared after sequence DROP preflight"
                )));
            }
        }
        Ok(())
    }

    /// Remove schema-expression dependencies requested by `DROP SEQUENCE CASCADE` and always detach serial ownership metadata whose sequence is being removed.
    pub(crate) fn detach_sequence_column_dependencies(
        &self,
        sequence: &str,
        cascade: bool,
    ) -> StorageBackendResult<()> {
        let dependencies = self.sequence_schema_expression_dependents(sequence)?;
        if !cascade && !dependencies.is_empty() {
            self.ensure_no_sequence_schema_dependencies(sequence)?;
        }
        if cascade {
            self.drop_sequence_schema_dependencies(&dependencies)?;
        }
        let mut catalog_changed = false;
        for (table_name, table) in self.table_entries() {
            let mut columns = table.columns.read().clone();
            let mut table_changed = false;
            for column in &mut columns {
                if column
                    .auto_increment
                    .as_ref()
                    .is_some_and(|provenance| provenance.sequence.as_deref() == Some(sequence))
                {
                    column.auto_increment = None;
                    table_changed = true;
                }
            }
            if !table_changed {
                continue;
            }
            if self.is_persistent() {
                self.try_save_table_schema_with_columns(&table_name, &table, &columns)?;
            }
            *table.columns.write() = columns;
            catalog_changed = true;
        }
        if catalog_changed {
            self.note_table_catalog_changed();
        }
        self.foreign_definition_context()
            .detach_foreign_table_sequence_provenance(sequence)?;
        Ok(())
    }
}
