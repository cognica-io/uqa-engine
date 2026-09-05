//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Sequence identities and lifecycle dependencies in stored schema expressions.

use crate::engine_sequences::SequenceSchemaDependent;
use crate::{Engine, RelationIdentity, StorageBackendError, StorageBackendResult};

use super::super::{rewrite_sequence_function_references, stored_relation_reference_matches};

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

fn rewrite_sequence_schema_references(
    columns: &mut [uqa_sql::ast::ColumnDef],
    checks: &mut [uqa_sql::ast::TableCheck],
    from: &RelationIdentity,
    to: &str,
) -> StorageBackendResult<bool> {
    let mut changed = false;
    for column in columns {
        if let Some(sequence) = column
            .auto_increment
            .as_mut()
            .and_then(|provenance| provenance.sequence.as_mut())
        {
            if stored_relation_reference_matches(sequence, from) {
                *sequence = to.to_string();
                changed = true;
            }
        }
        for expression in [&mut column.default, &mut column.check]
            .into_iter()
            .flatten()
        {
            rewrite_sequence_function_references(expression, &mut |reference| {
                if stored_relation_reference_matches(reference, from) {
                    *reference = to.to_string();
                    changed = true;
                }
                Ok(())
            })?;
        }
        if let Some(generated) = &mut column.generated {
            rewrite_sequence_function_references(&mut generated.expression, &mut |reference| {
                if stored_relation_reference_matches(reference, from) {
                    *reference = to.to_string();
                    changed = true;
                }
                Ok(())
            })?;
        }
    }
    for check in checks {
        rewrite_sequence_function_references(&mut check.expr, &mut |reference| {
            if stored_relation_reference_matches(reference, from) {
                *reference = to.to_string();
                changed = true;
            }
            Ok(())
        })?;
    }
    Ok(changed)
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
    pub(crate) fn bind_sequence_references_in_expr(
        &self,
        expression: &mut uqa_sql::ast::Expr,
    ) -> StorageBackendResult<()> {
        rewrite_sequence_function_references(expression, &mut |reference| {
            *reference = self.resolve_sequence_reference_for_binding(reference)?;
            Ok(())
        })
    }

    pub(in crate::engine_table_storage) fn resolve_stored_sequence_references_in_expr(
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

    pub(crate) fn resolve_loaded_sequence_references_in_expr(
        &self,
        expression: &mut uqa_sql::ast::Expr,
    ) -> StorageBackendResult<()> {
        rewrite_sequence_function_references(expression, &mut |reference| {
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

    pub(crate) fn rewrite_sequence_schema_dependencies(
        &self,
        from: &RelationIdentity,
        to: &str,
    ) -> StorageBackendResult<()> {
        let mut table_updates = Vec::new();
        for (table_name, table) in self.table_entries() {
            let mut columns = table.columns.read().clone();
            let mut checks = table.table_checks.read().clone();
            let foreign_keys = table.foreign_keys.read().clone();
            let key_constraints = table.key_constraints.read().clone();
            let hierarchy = table.hierarchy.read().clone();
            if rewrite_sequence_schema_references(&mut columns, &mut checks, from, to)? {
                table_updates.push((
                    table_name,
                    table,
                    columns,
                    checks,
                    foreign_keys,
                    key_constraints,
                    hierarchy,
                ));
            }
        }
        let mut foreign_updates = Vec::new();
        for (relation, mut table) in self.durable.foreign_tables.read().clone() {
            if rewrite_sequence_schema_references(&mut table.columns, &mut table.checks, from, to)?
            {
                foreign_updates.push((relation, table));
            }
        }
        for (table_name, table, columns, checks, foreign_keys, key_constraints, hierarchy) in
            &table_updates
        {
            self.persist_constraint_candidate_with_hierarchy(
                table_name,
                table,
                columns,
                checks,
                foreign_keys,
                key_constraints,
                hierarchy,
            )?;
        }
        for (relation, table) in &foreign_updates {
            self.persist_foreign_table_definition(relation, table)?;
        }
        for (_, table, columns, checks, _, _, _) in &table_updates {
            (*table.columns.write()).clone_from(columns);
            (*table.table_checks.write()).clone_from(checks);
        }
        let foreign_tables_changed = !foreign_updates.is_empty();
        if foreign_tables_changed {
            let mut tables = self.durable.foreign_tables.write();
            for (relation, table) in foreign_updates {
                tables.insert(relation, table);
            }
        }
        if !table_updates.is_empty() {
            self.note_table_catalog_changed();
        }
        if foreign_tables_changed {
            self.note_catalog_registry_changed();
        }
        Ok(())
    }

    /// Retire the legacy name-based owner marker after an explicit `ALTER SEQUENCE ... OWNED BY` action. Generation provenance and the bound sequence reference remain on the original SERIAL or identity column, while the stable dependency stored on the sequence becomes authoritative for lifecycle operations.
    pub(crate) fn clear_auto_increment_owner_markers(
        &self,
        sequence: &str,
    ) -> StorageBackendResult<()> {
        let target =
            RelationIdentity::from_legacy_name(sequence).map_err(StorageBackendError::Other)?;
        let mut catalog_changed = false;
        for (table_name, table) in self.table_entries() {
            let mut columns = table.columns.read().clone();
            let mut changed = false;
            for column in &mut columns {
                let Some(provenance) = column.auto_increment.as_mut() else {
                    continue;
                };
                if provenance.owner.is_some()
                    && provenance.sequence.as_deref().is_some_and(|reference| {
                        stored_relation_reference_matches(reference, &target)
                    })
                {
                    provenance.owner = None;
                    changed = true;
                }
            }
            if !changed {
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
        let mut foreign_updates = Vec::new();
        for (relation, mut table) in self.durable.foreign_tables.read().clone() {
            let mut changed = false;
            for column in &mut table.columns {
                let Some(provenance) = column.auto_increment.as_mut() else {
                    continue;
                };
                if provenance.owner.is_some()
                    && provenance.sequence.as_deref().is_some_and(|reference| {
                        stored_relation_reference_matches(reference, &target)
                    })
                {
                    provenance.owner = None;
                    changed = true;
                }
            }
            if changed {
                foreign_updates.push((relation, table));
            }
        }
        for (relation, table) in &foreign_updates {
            self.persist_foreign_table_definition(relation, table)?;
        }
        if !foreign_updates.is_empty() {
            let mut tables = self.durable.foreign_tables.write();
            for (relation, table) in foreign_updates {
                tables.insert(relation, table);
            }
            drop(tables);
            self.note_catalog_registry_changed();
        }
        Ok(())
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
                if self.drop_foreign_table_check_dependency(table, constraint)? != Some(true) {
                    return Err(StorageBackendError::Other(format!(
                        "constraint `{constraint}` on foreign table `{table}` disappeared after sequence DROP preflight"
                    )));
                }
            } else {
                crate::sql::drop_constraint_dependency(self, table, constraint)
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
                self.clear_foreign_table_default_dependency(table, column)? == Some(true)
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
                self.drop_foreign_table_generated_column_dependency(table, column)? == Some(true)
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
        self.detach_foreign_table_sequence_provenance(sequence)?;
        Ok(())
    }
}
